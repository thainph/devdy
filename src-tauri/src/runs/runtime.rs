//! Live per-run activity for the Runtime Dashboard (`get_runtime_snapshot`).
//!
//! The stdout drain (`runs::sidecar`) feeds every parsed line of a run in here;
//! the dashboard reads a cheap summary back: when the run last produced an
//! event, an events-per-minute ring buffer, the tool it is running now, its
//! current context occupancy and, once it ends, a short failure reason. Nothing
//! is persisted and no log is re-read. Codex output is already translated into
//! Claude-shaped stream-json by `sidecar-codex`, so one parser covers both.
//!
//! Everything kept here is a short, sanitized summary: tool inputs and error
//! texts go through [`sanitize`] (env assignments, auth headers, token-looking
//! strings, URL credentials and query strings are masked) and are truncated.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// Minutes covered by the events-per-minute ring buffer.
pub const BUCKETS: usize = 20;
/// Longest tool summary returned to the UI (chars).
pub const SUMMARY_MAX: usize = 120;
/// Longest failure reason kept for a run (chars).
const ERROR_MAX: usize = 160;
/// Open tool calls remembered per run (parallel calls, subagents).
const MAX_OPEN_TOOLS: usize = 32;
/// Ended runs whose activity (failure reason, last context size) is kept.
const MAX_ENDED: usize = 64;

/// Events per minute over the last [`BUCKETS`] minutes, as a fixed ring.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MinuteBuckets {
    counts: [u32; BUCKETS],
    /// Newest minute (unix minutes) the ring has advanced to.
    last: Option<i64>,
}

fn slot(minute: i64) -> usize {
    minute.rem_euclid(BUCKETS as i64) as usize
}

impl MinuteBuckets {
    /// Count one event at `minute` (unix minutes). Events older than the window
    /// (a clock step back) are dropped.
    pub fn record(&mut self, minute: i64) {
        self.advance(minute);
        let last = self.last.unwrap_or(minute);
        if last - minute < BUCKETS as i64 {
            let c = &mut self.counts[slot(minute)];
            *c = c.saturating_add(1);
        }
    }

    /// Move the ring forward to `minute`, zeroing the minutes skipped over.
    fn advance(&mut self, minute: i64) {
        match self.last {
            None => {
                self.counts = [0; BUCKETS];
                self.last = Some(minute);
            }
            Some(last) if minute > last => {
                let gap = (minute - last).min(BUCKETS as i64);
                for i in 1..=gap {
                    self.counts[slot(last + i)] = 0;
                }
                self.last = Some(minute);
            }
            _ => {}
        }
    }

    /// Exactly [`BUCKETS`] counts ending at `now_minute`, oldest first.
    pub fn series(&self, now_minute: i64) -> Vec<u32> {
        let Some(last) = self.last else {
            return vec![0; BUCKETS];
        };
        (0..BUCKETS as i64)
            .map(|i| {
                let m = now_minute - (BUCKETS as i64 - 1) + i;
                if m > last || last - m >= BUCKETS as i64 {
                    0
                } else {
                    self.counts[slot(m)]
                }
            })
            .collect()
    }
}

pub fn unix_minute(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(60)
}

/// A tool call that has started and has no `tool_result` yet.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenTool {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub started_at: DateTime<Utc>,
}

/// Everything tracked for one run.
#[derive(Debug, Clone, Default)]
pub struct RunActivity {
    /// Codex: its `fileChange` is translated into an `Edit` tool_use that never
    /// gets a `tool_result` (it is emitted once already applied).
    codex: bool,
    pub last_event_at: Option<DateTime<Utc>>,
    buckets: MinuteBuckets,
    /// Oldest first.
    open_tools: Vec<OpenTool>,
    pub context_tokens: Option<u64>,
    /// A per-message assistant usage set `context_tokens` (the turn total of a
    /// `result` is then no longer used as a fallback).
    saw_message_usage: bool,
    /// Why the run failed (latest unresolved error), sanitized.
    pub last_error: Option<String>,
    ended_at: Option<DateTime<Utc>>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// input + cache read + cache creation of a usage object (the prompt the model
/// saw, i.e. the context window occupancy). None when absent or zero.
fn context_of(usage: Option<&Value>) -> Option<u64> {
    let u = usage?;
    let tok = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let total =
        tok("input_tokens") + tok("cache_read_input_tokens") + tok("cache_creation_input_tokens");
    (total > 0).then_some(total)
}

impl RunActivity {
    pub fn new(engine: &str) -> Self {
        Self {
            codex: engine == "codex",
            ..Self::default()
        }
    }

    /// Fold one parsed stdout line of the run in.
    pub fn observe(&mut self, v: &Value, now: DateTime<Utc>) {
        let kind = str_of(v, "type").unwrap_or("");
        if let Some(control) = kind.strip_prefix("_devdy_") {
            match control {
                "error" => {
                    let text = str_of(v, "text")
                        .or_else(|| str_of(v, "error"))
                        .unwrap_or("");
                    self.note_error(text);
                }
                // A prompt waiting for the user is the run doing something.
                "permission_request" => self.last_event_at = Some(now),
                _ => {}
            }
            return;
        }
        self.last_event_at = Some(now);
        // Partial-message deltas show the run is alive but would swamp the
        // per-minute counts (one per token chunk).
        if kind == "stream_event" {
            return;
        }
        self.buckets.record(unix_minute(now));

        let subagent = !matches!(v.get("parent_tool_use_id"), None | Some(Value::Null));
        match kind {
            "assistant" => {
                let message = v.get("message");
                for block in message
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                {
                    if str_of(block, "type") == Some("tool_use") {
                        self.open_tool(block, now);
                    }
                }
                // A subagent's usage is its own context, not the run's.
                if !subagent {
                    if let Some(ctx) = context_of(message.and_then(|m| m.get("usage"))) {
                        self.context_tokens = Some(ctx);
                        self.saw_message_usage = true;
                    }
                }
            }
            "user" => {
                for block in v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                {
                    if str_of(block, "type") == Some("tool_result") {
                        if let Some(id) = str_of(block, "tool_use_id") {
                            self.open_tools.retain(|t| t.id != id);
                        }
                    }
                }
            }
            "system" if str_of(v, "subtype") == Some("compact_boundary") => {
                self.context_tokens = Some(0);
                self.saw_message_usage = false;
            }
            "result" => {
                // The turn is over: nothing it started is still running in it.
                self.open_tools.clear();
                if str_of(v, "subtype") == Some("success")
                    && v.get("is_error") != Some(&Value::Bool(true))
                {
                    self.last_error = None;
                } else if v.get("is_error") == Some(&Value::Bool(true))
                    || str_of(v, "subtype").is_some_and(|s| s.starts_with("error"))
                {
                    let text = str_of(v, "result")
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| str_of(v, "subtype"))
                        .unwrap_or("error");
                    self.note_error(text);
                }
                // Engines without per-message usage (codex): the turn's usage
                // is the best estimate there is.
                if !self.saw_message_usage {
                    if let Some(ctx) = context_of(v.get("usage")) {
                        self.context_tokens = Some(ctx);
                    }
                }
            }
            _ => {}
        }
    }

    fn open_tool(&mut self, block: &Value, now: DateTime<Utc>) {
        let name = str_of(block, "name").unwrap_or("tool");
        if self.codex && name == "Edit" {
            return;
        }
        let Some(id) = str_of(block, "id") else {
            return;
        };
        let input = block.get("input").unwrap_or(&Value::Null);
        self.open_tools.retain(|t| t.id != id);
        if self.open_tools.len() >= MAX_OPEN_TOOLS {
            self.open_tools.remove(0);
        }
        self.open_tools.push(OpenTool {
            id: id.to_string(),
            name: truncate(name, 80),
            summary: tool_summary(name, input),
            started_at: now,
        });
    }

    /// The latest tool call still without a result.
    pub fn current_tool(&self) -> Option<&OpenTool> {
        self.open_tools.last()
    }

    pub fn events_per_minute(&self, now: DateTime<Utc>) -> Vec<u32> {
        self.buckets.series(unix_minute(now))
    }

    pub fn note_error(&mut self, text: &str) {
        let clean = truncate(&sanitize(text), ERROR_MAX);
        if !clean.is_empty() {
            self.last_error = Some(clean);
        }
    }
}

// ---- tool summaries ----------------------------------------------------------

fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
}

/// Short human text for a tool call: the command, file, pattern or query it
/// works on. Only well-known fields are read (never env, headers or arbitrary
/// MCP arguments), then sanitized and truncated to [`SUMMARY_MAX`].
pub fn tool_summary(name: &str, input: &Value) -> String {
    let field = |k: &str| str_of(input, k).filter(|s| !s.trim().is_empty());
    let raw: String = match name {
        "Bash" | "BashOutput" | "KillShell" => field("command")
            .map(|c| {
                c.lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string()
            })
            .or_else(|| field("description").map(str::to_string))
            .unwrap_or_default(),
        "Read" | "Write" | "Edit" | "MultiEdit" => {
            match field("file_path").or_else(|| field("path")) {
                Some(p) => basename(p).to_string(),
                // Codex `fileChange`: `changes: [{ path, kind }]`.
                None => {
                    let paths: Vec<&str> = input
                        .get("changes")
                        .and_then(|c| c.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|c| str_of(c, "path"))
                        .collect();
                    match paths.as_slice() {
                        [] => String::new(),
                        [one] => basename(one).to_string(),
                        [first, rest @ ..] => format!("{} +{}", basename(first), rest.len()),
                    }
                }
            }
        }
        "NotebookEdit" | "NotebookRead" => field("notebook_path")
            .map(basename)
            .unwrap_or("")
            .to_string(),
        "Glob" | "Grep" => field("pattern").unwrap_or("").to_string(),
        "WebFetch" => field("url").map(strip_url).unwrap_or_default(),
        "WebSearch" => field("query").unwrap_or("").to_string(),
        "Task" | "Agent" => field("description")
            .or_else(|| field("subagent_type"))
            .unwrap_or("")
            .to_string(),
        "AskUserQuestion" => input
            .get("questions")
            .and_then(|q| q.as_array())
            .and_then(|q| q.first())
            .and_then(|q| str_of(q, "question"))
            .unwrap_or("")
            .to_string(),
        "TodoWrite" => input
            .get("todos")
            .and_then(|t| t.as_array())
            .map(|t| format!("{} todos", t.len()))
            .unwrap_or_default(),
        _ => [
            "description",
            "file_path",
            "path",
            "pattern",
            "query",
            "title",
            "name",
        ]
        .iter()
        .find_map(|k| field(k))
        .map(|s| {
            if s.contains("://") {
                strip_url(s)
            } else {
                s.to_string()
            }
        })
        .unwrap_or_default(),
    };
    truncate(&sanitize(&raw), SUMMARY_MAX)
}

// ---- sanitizing --------------------------------------------------------------

const MASK: &str = "***";

/// Prefixes of well-known credential formats.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "gldt-",
    "sk-",
    "sk_live_",
    "rk_live_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "xapp-",
    "AKIA",
    "ASIA",
    "eyJ",
    "ya29.",
    "AIza",
];

/// Header names / words after which the next word is a credential.
const SECRET_LEADERS: &[&str] = &[
    "authorization:",
    "bearer",
    "basic",
    "x-api-key:",
    "private-token:",
    "cookie:",
    "token",
    "apikey:",
];

/// Key words that make a `key=value` / `--key value` a secret.
const SECRET_KEYS: &[&str] = &[
    "token", "secret", "pass", "pwd", "auth", "cred", "cookie", "apikey", "api_key", "api-key",
    "private", "session", "key",
];

fn is_secret_key(key: &str) -> bool {
    let k = key.trim_start_matches('-').to_ascii_lowercase();
    SECRET_KEYS.iter().any(|w| k.contains(w))
}

/// An env-var style name (`FOO_BAR`, `GH_TOKEN`): upper-case identifier.
fn is_env_name(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Strip quotes/brackets around a word for classification.
fn core(word: &str) -> &str {
    word.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
    })
}

fn looks_like_token(word: &str) -> bool {
    let w = core(word);
    if TOKEN_PREFIXES
        .iter()
        .any(|p| w.starts_with(p) && w.len() >= p.len() + 8)
    {
        return true;
    }
    // Long opaque strings (keys, hashes): letters and digits, no path parts.
    w.len() >= 32
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '=')
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

/// A URL without credentials, query string or fragment.
fn strip_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let rest = match rest.find('/') {
        Some(slash) => {
            let (authority, path) = rest.split_at(slash);
            format!(
                "{}{}",
                authority.rsplit('@').next().unwrap_or(authority),
                path
            )
        }
        None => rest.rsplit('@').next().unwrap_or(rest).to_string(),
    };
    format!("{scheme}{rest}")
}

/// Collapse whitespace and mask anything that may be a credential: values of
/// env assignments and secret-named flags, the word after an auth header,
/// token-looking strings, and URL credentials / query strings.
pub fn sanitize(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut mask_next = false;
    for word in text.split_whitespace() {
        let c = core(word);
        let lower = c.to_ascii_lowercase();
        let leader = SECRET_LEADERS
            .iter()
            .any(|l| lower == *l || lower.ends_with(&format!("-{l}")));
        if mask_next {
            // `Authorization: Bearer <token>`: the scheme word is kept, the
            // credential after it is masked.
            if leader {
                out.push(word.to_string());
                continue;
            }
            mask_next = false;
            out.push(MASK.to_string());
            continue;
        }
        if leader {
            out.push(word.to_string());
            mask_next = true;
            continue;
        }
        // `--token value` / `--password value`.
        if c.starts_with("--") && !c.contains('=') && is_secret_key(c) {
            out.push(word.to_string());
            mask_next = true;
            continue;
        }
        if c.contains("://") {
            out.push(strip_url(c));
            continue;
        }
        if let Some((key, value)) = c.split_once('=') {
            let bare = key.trim_start_matches('-');
            let keyish = !bare.is_empty()
                && bare
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.');
            if keyish
                && !value.is_empty()
                && (is_env_name(bare) || is_secret_key(bare) || looks_like_token(value))
            {
                out.push(format!("{key}={MASK}"));
                continue;
            }
        }
        if looks_like_token(word) {
            out.push(MASK.to_string());
            continue;
        }
        out.push(word.to_string());
    }
    out.join(" ")
}

/// At most `max` chars, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

// ---- process-wide store ------------------------------------------------------

fn store() -> &'static Mutex<HashMap<String, RunActivity>> {
    static STORE: OnceLock<Mutex<HashMap<String, RunActivity>>> = OnceLock::new();
    STORE.get_or_init(Mutex::default)
}

fn with_store<T>(f: impl FnOnce(&mut HashMap<String, RunActivity>) -> T) -> T {
    let mut map = store().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut map)
}

/// A drain started for `run_id` (new run, resume or follow-up turn). Context
/// size and the per-minute counts carry over from the previous turn.
pub fn begin(run_id: &str, engine: &str) {
    with_store(|m| {
        let a = m
            .entry(run_id.to_string())
            .or_insert_with(|| RunActivity::new(engine));
        a.codex = engine == "codex";
        a.open_tools.clear();
        a.ended_at = None;
    });
}

/// Fold one parsed stdout line of `run_id` in.
pub fn observe(run_id: &str, v: &Value) {
    let now = Utc::now();
    with_store(|m| {
        m.entry(run_id.to_string()).or_default().observe(v, now);
    });
}

/// The run's drain ended with `status`. A failure keeps its reason (the latest
/// unresolved error, else `fallback`, e.g. the exit code); any other outcome
/// clears it.
pub fn end(run_id: &str, status: &str, fallback: Option<&str>) {
    let now = Utc::now();
    with_store(|m| {
        let a = m.entry(run_id.to_string()).or_default();
        a.open_tools.clear();
        a.ended_at = Some(now);
        if status == "failed" {
            if a.last_error.is_none() {
                if let Some(f) = fallback {
                    a.note_error(f);
                }
            }
        } else {
            a.last_error = None;
        }
    });
}

/// A copy of the run's activity.
pub fn get(run_id: &str) -> Option<RunActivity> {
    with_store(|m| m.get(run_id).cloned())
}

/// Why `run_id` failed, if this app session saw it fail.
pub fn last_error(run_id: &str) -> Option<String> {
    with_store(|m| m.get(run_id).and_then(|a| a.last_error.clone()))
}

/// Forget runs that are no longer live, keeping the [`MAX_ENDED`] most
/// recently ended ones (their failure reasons feed the "recent" list).
pub fn prune(live: &HashSet<String>) {
    with_store(|m| prune_map(m, live));
}

fn prune_map(m: &mut HashMap<String, RunActivity>, live: &HashSet<String>) {
    let mut ended: Vec<(DateTime<Utc>, String)> = m
        .iter()
        .filter(|(id, _)| !live.contains(*id))
        .map(|(id, a)| {
            (
                a.ended_at.or(a.last_event_at).unwrap_or_default(),
                id.clone(),
            )
        })
        .collect();
    if ended.len() <= MAX_ENDED {
        return;
    }
    ended.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (_, id) in ended.into_iter().skip(MAX_ENDED) {
        m.remove(&id);
    }
}

/// Short reason for a sidecar that ended badly without reporting an error.
pub fn exit_reason(
    code: Option<i64>,
    signal: Option<&str>,
    exit_ok: Option<bool>,
) -> Option<String> {
    if let Some(sig) = signal {
        return Some(format!("Engine killed by {sig}"));
    }
    match code {
        Some(c) if c != 0 => Some(format!("Engine exited with code {c}")),
        _ if exit_ok == Some(false) => Some("Session process exited abnormally".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn at(min: i64, sec: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 / 60 * 60 + min * 60 + sec, 0)
            .unwrap()
    }

    #[test]
    fn ring_buffer_counts_and_rolls() {
        let mut b = MinuteBuckets::default();
        assert_eq!(b.series(100), vec![0; BUCKETS]);
        b.record(100);
        b.record(100);
        b.record(101);
        let s = b.series(101);
        assert_eq!(s.len(), BUCKETS);
        assert_eq!(s[BUCKETS - 1], 1);
        assert_eq!(s[BUCKETS - 2], 2);
        assert_eq!(s.iter().sum::<u32>(), 3);
        // Reading later shifts the window without new events.
        let s = b.series(105);
        assert_eq!(s[BUCKETS - 5], 1);
        assert_eq!(s[BUCKETS - 6], 2);
        assert_eq!(s[BUCKETS - 1], 0);
        // A gap shorter than the window clears only skipped minutes.
        b.record(110);
        let s = b.series(110);
        assert_eq!(s[BUCKETS - 1], 1);
        assert_eq!(s[BUCKETS - 10], 1);
        assert_eq!(s[BUCKETS - 11], 2);
        assert_eq!(s.iter().sum::<u32>(), 4);
    }

    #[test]
    fn ring_buffer_expires_old_minutes() {
        let mut b = MinuteBuckets::default();
        b.record(100);
        assert_eq!(b.series(119)[0], 1);
        assert_eq!(b.series(120), vec![0; BUCKETS]);
        // A gap longer than the window resets everything.
        b.record(200);
        let s = b.series(200);
        assert_eq!(s.iter().sum::<u32>(), 1);
        assert_eq!(s[BUCKETS - 1], 1);
        // Late event within the window lands in its own minute; older is dropped.
        b.record(195);
        b.record(150);
        let s = b.series(200);
        assert_eq!(s[BUCKETS - 6], 1);
        assert_eq!(s.iter().sum::<u32>(), 2);
        // Negative minutes don't panic.
        let mut n = MinuteBuckets::default();
        n.record(-3);
        assert_eq!(n.series(-3)[BUCKETS - 1], 1);
    }

    #[test]
    fn counts_events_but_not_deltas_or_heartbeats() {
        let mut a = RunActivity::new("claude");
        a.observe(&json!({"type": "_devdy_heartbeat"}), at(0, 1));
        assert!(a.last_event_at.is_none());
        a.observe(&json!({"type": "stream_event", "event": {}}), at(0, 2));
        assert_eq!(a.last_event_at, Some(at(0, 2)));
        assert_eq!(a.events_per_minute(at(0, 3)).iter().sum::<u32>(), 0);
        a.observe(&json!({"type": "system", "subtype": "init"}), at(0, 4));
        a.observe(
            &json!({"type": "assistant", "message": {"content": []}}),
            at(1, 0),
        );
        let s = a.events_per_minute(at(1, 30));
        assert_eq!(&s[BUCKETS - 2..], &[1, 1]);
        assert_eq!(a.last_event_at, Some(at(1, 0)));
    }

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
    }

    fn tool_result(id: &str) -> Value {
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
    }

    #[test]
    fn tracks_latest_open_tool() {
        let mut a = RunActivity::new("claude");
        a.observe(
            &tool_use("t1", "Task", json!({"description": "Explore repo"})),
            at(0, 0),
        );
        a.observe(
            &json!({"type": "assistant", "parent_tool_use_id": "t1", "message": {"content": [
                {"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/x/src/registry.rs"}}
            ]}}),
            at(0, 5),
        );
        let cur = a.current_tool().unwrap();
        assert_eq!(
            (cur.name.as_str(), cur.summary.as_str()),
            ("Read", "registry.rs")
        );
        assert_eq!(cur.started_at, at(0, 5));
        // The subagent's tool finishes: back to the Task that is still running.
        a.observe(&tool_result("t2"), at(0, 6));
        assert_eq!(a.current_tool().unwrap().summary, "Explore repo");
        a.observe(&tool_result("t1"), at(0, 7));
        assert!(a.current_tool().is_none());
        // A turn end closes whatever is left.
        a.observe(
            &tool_use("t3", "Bash", json!({"command": "pnpm test\necho done"})),
            at(0, 8),
        );
        assert_eq!(a.current_tool().unwrap().summary, "pnpm test");
        a.observe(&json!({"type": "result", "subtype": "success"}), at(0, 9));
        assert!(a.current_tool().is_none());
    }

    #[test]
    fn codex_edit_is_not_left_open() {
        let mut a = RunActivity::new("codex");
        a.observe(
            &tool_use("e1", "Edit", json!({"changes": [{"path": "/a/b.rs"}]})),
            at(0, 0),
        );
        assert!(a.current_tool().is_none());
        a.observe(&tool_use("c1", "Bash", json!({"command": "ls"})), at(0, 1));
        assert_eq!(a.current_tool().unwrap().name, "Bash");
    }

    #[test]
    fn summaries_per_tool() {
        assert_eq!(
            tool_summary("Edit", &json!({"file_path": "/repo/src-tauri/src/lib.rs"})),
            "lib.rs"
        );
        assert_eq!(
            tool_summary(
                "Edit",
                &json!({"changes": [{"path": "/r/a.rs"}, {"path": "/r/b.rs"}, {"path": "/r/c.rs"}]})
            ),
            "a.rs +2"
        );
        assert_eq!(
            tool_summary("Grep", &json!({"pattern": "fn main", "path": "/x"})),
            "fn main"
        );
        assert_eq!(
            tool_summary(
                "WebFetch",
                &json!({"url": "https://user:pw@example.com/a/b?access_token=abc#x"})
            ),
            "https://example.com/a/b"
        );
        assert_eq!(
            tool_summary(
                "AskUserQuestion",
                &json!({"questions": [{"question": "Which DB?", "options": []}]})
            ),
            "Which DB?"
        );
        assert_eq!(
            tool_summary("TodoWrite", &json!({"todos": [{}, {}]})),
            "2 todos"
        );
        assert_eq!(
            tool_summary("mcp__devdy__todos_list", &json!({"project_id": "p1"})),
            ""
        );
        assert_eq!(tool_summary("Bash", &json!({})), "");
    }

    #[test]
    fn summary_is_truncated() {
        let long = "x".repeat(500);
        let s = tool_summary("Grep", &json!({ "pattern": long }));
        assert_eq!(s.chars().count(), SUMMARY_MAX);
        assert!(s.ends_with('…'));
        // Multi-byte text is cut on a char boundary.
        let s = tool_summary("WebSearch", &json!({ "query": "ữ".repeat(300) }));
        assert_eq!(s.chars().count(), SUMMARY_MAX);
    }

    #[test]
    fn summary_never_leaks_env_or_headers() {
        let input = json!({
            "command": "GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123 API_KEY=hunter2 curl -H 'Authorization: Bearer s3cr3tvalue' -H \"X-Api-Key: k123\" --token abc --password=pw https://bob:pw@api.x.com/v1?key=zzz gh pr list",
            "env": {"SECRET": "nope"},
            "headers": {"Authorization": "nope"}
        });
        let s = tool_summary("Bash", &input);
        for leak in [
            "ghp_",
            "hunter2",
            "s3cr3tvalue",
            "k123",
            "abc",
            "=pw",
            "bob",
            "zzz",
            "nope",
        ] {
            assert!(!s.contains(leak), "{leak} leaked in {s}");
        }
        assert!(s.contains("GH_TOKEN=***"));
        assert!(s.contains("curl"));
        assert_eq!(
            sanitize("curl https://bob:pw@api.x.com/v1?key=zzz"),
            "curl https://api.x.com/v1"
        );
        // Arbitrary MCP arguments (env / headers) are never read.
        let s = tool_summary(
            "mcp__srv__call",
            &json!({"env": {"TOKEN": "zzz"}, "headers": {"Authorization": "Bearer zzz"}}),
        );
        assert_eq!(s, "");
        // Lone token-looking strings are masked; ordinary words survive.
        assert_eq!(
            sanitize("push with xoxb-1234567890-abcdefgh and sk-ant-api03-abcdefghijk"),
            "push with *** and ***"
        );
        assert_eq!(
            sanitize("cargo test --release  --format=json"),
            "cargo test --release --format=json"
        );
    }

    #[test]
    fn context_tokens_follow_latest_message_and_reset_on_compaction() {
        let mut a = RunActivity::new("claude");
        let msg = |input: u64, read: u64, create: u64| {
            json!({"type": "assistant", "message": {"content": [], "usage": {
                "input_tokens": input, "cache_read_input_tokens": read,
                "cache_creation_input_tokens": create, "output_tokens": 999}}})
        };
        assert_eq!(a.context_tokens, None);
        a.observe(&msg(10, 50_000, 200), at(0, 0));
        assert_eq!(a.context_tokens, Some(50_210));
        a.observe(&msg(5, 40_000, 0), at(0, 1));
        assert_eq!(
            a.context_tokens,
            Some(40_005),
            "latest, not the high-water mark"
        );
        // A subagent's usage does not move the run's context.
        a.observe(
            &json!({"type": "assistant", "parent_tool_use_id": "t", "message": {"content": [], "usage": {"input_tokens": 1}}}),
            at(0, 2),
        );
        assert_eq!(a.context_tokens, Some(40_005));
        // The cumulative result total is ignored once per-message usage was seen.
        a.observe(
            &json!({"type": "result", "subtype": "success", "usage": {"input_tokens": 900_000}}),
            at(0, 3),
        );
        assert_eq!(a.context_tokens, Some(40_005));
        a.observe(
            &json!({"type": "system", "subtype": "compact_boundary"}),
            at(0, 4),
        );
        assert_eq!(a.context_tokens, Some(0));
        a.observe(&msg(1, 3_000, 0), at(0, 5));
        assert_eq!(a.context_tokens, Some(3_001));
    }

    #[test]
    fn codex_context_falls_back_to_turn_usage() {
        let mut a = RunActivity::new("codex");
        a.observe(
            &json!({"type": "result", "subtype": "success", "usage": {"input_tokens": 100, "cache_read_input_tokens": 2_000, "output_tokens": 50}}),
            at(0, 0),
        );
        assert_eq!(a.context_tokens, Some(2_100));
    }

    #[test]
    fn errors_are_kept_sanitized_and_cleared_by_success() {
        let mut a = RunActivity::new("claude");
        a.observe(&json!({"type": "_devdy_error", "text": "auth failed for GITHUB_TOKEN=ghp_zzzzzzzzzzzzzzzz"}), at(0, 0));
        let e = a.last_error.clone().unwrap();
        assert!(!e.contains("ghp_"), "{e}");
        a.observe(&json!({"type": "result", "subtype": "success"}), at(0, 1));
        assert!(a.last_error.is_none());
        a.observe(
            &json!({"type": "result", "subtype": "error_max_turns", "is_error": true}),
            at(0, 2),
        );
        assert_eq!(a.last_error.as_deref(), Some("error_max_turns"));
        a.note_error(&"y".repeat(1000));
        assert_eq!(a.last_error.as_ref().unwrap().chars().count(), ERROR_MAX);
    }

    #[test]
    fn store_lifecycle_and_prune() {
        let id = format!("rt-test-{}", uuid::Uuid::new_v4());
        begin(&id, "claude");
        observe(&id, &tool_use("t", "Bash", json!({"command": "ls"})));
        assert!(get(&id).unwrap().current_tool().is_some());
        end(&id, "failed", Some("Engine exited with code 3"));
        let a = get(&id).unwrap();
        assert!(a.current_tool().is_none());
        assert_eq!(
            last_error(&id).as_deref(),
            Some("Engine exited with code 3")
        );
        // A later successful turn clears the reason.
        begin(&id, "claude");
        end(&id, "done", None);
        assert!(last_error(&id).is_none());

        let mut m: HashMap<String, RunActivity> = HashMap::new();
        for i in 0..(MAX_ENDED + 5) {
            let mut a = RunActivity::new("claude");
            a.ended_at = Some(at(i as i64, 0));
            m.insert(format!("r{i}"), a);
        }
        m.insert("live".into(), RunActivity::new("claude"));
        let live: HashSet<String> = ["live".to_string()].into();
        prune_map(&mut m, &live);
        assert_eq!(m.len(), MAX_ENDED + 1);
        assert!(m.contains_key("live"));
        assert!(!m.contains_key("r0"), "oldest ended runs go first");
        assert!(m.contains_key(&format!("r{}", MAX_ENDED + 4)));
    }

    #[test]
    fn exit_reasons() {
        assert_eq!(
            exit_reason(Some(2), None, None).as_deref(),
            Some("Engine exited with code 2")
        );
        assert_eq!(
            exit_reason(None, Some("SIGKILL"), None).as_deref(),
            Some("Engine killed by SIGKILL")
        );
        assert!(exit_reason(Some(0), None, Some(true)).is_none());
        assert!(exit_reason(None, None, Some(false)).is_some());
    }
}
