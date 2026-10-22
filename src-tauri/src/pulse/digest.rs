//! AI digest of one source (BR-06/07, pure): prompt, output parsing and
//! validation against the §5.2 schema, and the hard facts that override the
//! AI (closed issue, Slack ball rule, board deadline).

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use serde::Serialize;
use serde_json::{Map, Value};

pub const STATUSES: [&str; 5] = ["on_track", "at_risk", "blocked", "waiting", "done"];
pub const BALLS: [&str; 4] = ["me", "customer", "team", "none"];
const SUMMARY_MAX: usize = 500;
const NEXT_ACTION_MAX: usize = 200;
const EVIDENCE_MAX: usize = 200;
const TEXT_MAX: usize = 300;
const LIST_MAX: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Deadline {
    pub title: String,
    /// RFC3339.
    pub due_at: String,
    pub all_day: bool,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    pub text: String,
    pub decided_at: Option<String>,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Question {
    pub text: String,
    /// `me` | `other`.
    pub asked_by: String,
    pub evidence: Option<String>,
}

/// A validated digest (before it is stored).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DigestDraft {
    pub status: String,
    pub ball: String,
    pub ball_since: Option<String>,
    pub summary: String,
    pub next_action: Option<String>,
    pub deadlines: Vec<Deadline>,
    pub decisions: Vec<Decision>,
    pub open_questions: Vec<Question>,
}

/// What the prompt needs besides the content.
pub struct PromptCtx<'a> {
    /// `slack` | `github_issue`.
    pub source_type: &'a str,
    pub title: &'a str,
    /// How the user appears in the content (Slack: `name(me)`, GitHub: login).
    pub me: &'a str,
    pub now: DateTime<FixedOffset>,
    pub content: &'a str,
    pub truncated: bool,
}

const SCHEMA: &str = r#"{
  "status": "on_track | at_risk | blocked | waiting | done",
  "ball": "me | customer | team | none",
  "ball_since": "RFC3339 time of the message that put the ball there, or null",
  "summary": "1-3 sentences, Vietnamese, max 500 characters",
  "next_action": "the next concrete step, Vietnamese, max 200 characters, or null",
  "deadlines": [{ "title": "Vietnamese", "due_at": "YYYY-MM-DD or RFC3339 with offset", "all_day": true, "evidence": "verbatim quote from the source, max 200 characters" }],
  "decisions": [{ "text": "Vietnamese", "decided_at": "YYYY-MM-DD or null", "evidence": "verbatim quote, max 200 characters" }],
  "open_questions": [{ "text": "Vietnamese", "asked_by": "me | other", "evidence": "verbatim quote, max 200 characters" }]
}"#;

pub fn build_prompt(ctx: &PromptCtx) -> String {
    let source = if ctx.source_type == "slack" {
        "a Slack thread (one line per message: [time] author(me|other): text)"
    } else {
        "a GitHub issue (header, body, then comments)"
    };
    let truncated = if ctx.truncated {
        "\nThe source was too long: the first entry and the newest entries are kept, the middle is omitted.\n"
    } else {
        ""
    };
    format!(
        "You track project progress for one user. Read the source below and summarise where it stands.\n\
         This is a new, independent task: ignore any earlier conversation.\n\
         \n\
         Current date/time: {now} ({weekday}).\n\
         The user is: {me}. In Slack content, lines marked (me) are the user's own messages.\n\
         Source: {source}, titled \"{title}\".\n\
         \n\
         Rules:\n\
         - Answer with ONE JSON object only — no markdown fences, no text before or after.\n\
         - Write summary, next_action, titles, decisions and questions in Vietnamese.\n\
         - status: on_track = progressing normally; at_risk = a deadline or commitment looks threatened; \
           blocked = cannot move until something external happens; waiting = waiting on someone's answer \
           or input; done = finished/resolved.\n\
         - ball: who must act next — me (the user), customer (the client / requester side), team \
           (colleagues other than the user), none (nobody / finished).\n\
         - deadlines: only dates someone actually committed to. Every deadline and decision MUST carry \
           \"evidence\": a verbatim quote (max 200 characters) from the source; leave the item out if \
           you cannot quote it.\n\
         - A date without a year means the nearest such date on or after the date of the message that \
           mentions it.\n\
         - If a deadline was moved, keep only the newest one (by message time) in deadlines and add a \
           decision like \"Đã dời từ <old> sang <new>\".\n\
         - At most 10 items per list. Use empty lists when there is nothing.\n\
         {truncated}\n\
         JSON schema:\n{schema}\n\
         \n\
         Source content:\n\
         <<<SOURCE\n{content}\nSOURCE>>>\n",
        now = ctx.now.to_rfc3339(),
        weekday = ctx.now.format("%A"),
        me = ctx.me,
        title = ctx.title.replace('"', "'"),
        schema = SCHEMA,
        content = ctx.content,
    )
}

/// The prompt again, with the validation error of the previous answer.
pub fn retry_prompt(ctx: &PromptCtx, error: &str) -> String {
    format!(
        "{}\nYour previous answer was rejected: {error}. Reply again with ONLY the corrected JSON object.\n",
        build_prompt(ctx)
    )
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].trim_end().to_string(),
        None => s.to_string(),
    }
}

/// The JSON object in the answer: fences and chatter around it are tolerated.
fn extract_json(raw: &str) -> Result<Map<String, Value>, String> {
    let start = raw.find('{').ok_or("no JSON object in the answer")?;
    let end = raw.rfind('}').ok_or("no JSON object in the answer")?;
    if end < start {
        return Err("no JSON object in the answer".into());
    }
    match serde_json::from_str::<Value>(&raw[start..=end]) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("the answer is not a JSON object".into()),
        Err(e) => Err(format!("invalid JSON: {e}")),
    }
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn enum_field(m: &Map<String, Value>, key: &str, allowed: &[&str]) -> Result<String, String> {
    match m.get(key).and_then(Value::as_str) {
        Some(v) if allowed.contains(&v) => Ok(v.to_string()),
        Some(v) => Err(format!("\"{key}\" must be one of {allowed:?}, got \"{v}\"")),
        None => Err(format!("\"{key}\" is missing")),
    }
}

fn list<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a [Value], String> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a.as_slice()),
        Some(_) => Err(format!("\"{key}\" must be an array")),
    }
}

/// `YYYY-MM-DD` (all-day, midnight in `tz`) or an RFC3339 / naive datetime.
/// Returns `(rfc3339, all_day)`.
pub fn parse_due(s: &str, tz: FixedOffset) -> Option<(String, bool)> {
    let s = s.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = tz.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).single()?;
        return Some((dt.to_rfc3339(), true));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some((dt.to_rfc3339(), false));
    }
    for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return tz
                .from_local_datetime(&n)
                .single()
                .map(|dt| (dt.to_rfc3339(), false));
        }
    }
    None
}

fn parse_time(s: Option<String>, tz: FixedOffset) -> Option<String> {
    s.and_then(|s| parse_due(&s, tz)).map(|(t, _)| t)
}

/// Validate the AI answer against §5.2. Hard errors (not JSON, bad enums,
/// missing summary, lists of the wrong type) → `Err` for a retry; soft
/// problems are repaired: items without evidence / unparsable dates are
/// dropped, strings are trimmed to their limits, unknown fields ignored.
pub fn parse_digest(raw: &str, tz: FixedOffset) -> Result<DigestDraft, String> {
    let m = extract_json(raw)?;
    let status = enum_field(&m, "status", &STATUSES)?;
    let ball = enum_field(&m, "ball", &BALLS)?;
    let summary = opt_str(m.get("summary")).ok_or("\"summary\" is missing or empty")?;

    let mut deadlines = Vec::new();
    for d in list(&m, "deadlines")? {
        let (Some(title), Some(due), Some(evidence)) = (
            opt_str(d.get("title")),
            opt_str(d.get("due_at")),
            opt_str(d.get("evidence")),
        ) else {
            continue;
        };
        let Some((due_at, date_only)) = parse_due(&due, tz) else {
            continue;
        };
        let all_day = d
            .get("all_day")
            .and_then(Value::as_bool)
            .unwrap_or(date_only);
        deadlines.push(Deadline {
            title: truncate(&title, TEXT_MAX),
            due_at,
            all_day,
            evidence: truncate(&evidence, EVIDENCE_MAX),
        });
    }
    let mut decisions = Vec::new();
    for d in list(&m, "decisions")? {
        let (Some(text), Some(evidence)) = (opt_str(d.get("text")), opt_str(d.get("evidence")))
        else {
            continue;
        };
        decisions.push(Decision {
            text: truncate(&text, TEXT_MAX),
            decided_at: opt_str(d.get("decided_at")),
            evidence: truncate(&evidence, EVIDENCE_MAX),
        });
    }
    let mut open_questions = Vec::new();
    for q in list(&m, "open_questions")? {
        let Some(text) = opt_str(q.get("text")) else {
            continue;
        };
        let asked_by = match q.get("asked_by").and_then(Value::as_str) {
            Some("me") => "me",
            _ => "other",
        };
        open_questions.push(Question {
            text: truncate(&text, TEXT_MAX),
            asked_by: asked_by.to_string(),
            evidence: opt_str(q.get("evidence")).map(|e| truncate(&e, EVIDENCE_MAX)),
        });
    }
    deadlines.truncate(LIST_MAX);
    decisions.truncate(LIST_MAX);
    open_questions.truncate(LIST_MAX);
    Ok(DigestDraft {
        status,
        ball,
        ball_since: parse_time(opt_str(m.get("ball_since")), tz),
        summary: truncate(&summary, SUMMARY_MAX),
        next_action: opt_str(m.get("next_action")).map(|s| truncate(&s, NEXT_ACTION_MAX)),
        deadlines,
        decisions,
        open_questions,
    })
}

/// BR-07: facts that win over the AI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HardFacts {
    pub issue_closed: bool,
    /// RFC3339 of the message that put the ball in my court (Slack rule).
    pub ball_mine_since: Option<String>,
    /// `(title, YYYY-MM-DD)`: board deadline, else milestone due date.
    pub board_deadline: Option<(String, String)>,
}

pub fn apply_hard_facts(d: &mut DigestDraft, f: &HardFacts, tz: FixedOffset) {
    if f.issue_closed {
        d.status = "done".to_string();
    }
    if let Some(since) = &f.ball_mine_since {
        d.ball = "me".to_string();
        d.ball_since = Some(since.clone());
    }
    if let Some((title, date)) = &f.board_deadline {
        if let Some((due_at, all_day)) = parse_due(date, tz) {
            // The issue's own deadline replaces whatever the AI extracted.
            d.deadlines = vec![Deadline {
                title: title.clone(),
                due_at,
                all_day,
                evidence: format!("GitHub: {title} = {date}"),
            }];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(7 * 3600).unwrap()
    }

    const VALID: &str = r#"{
      "status": "at_risk", "ball": "customer", "ball_since": "2026-10-20T09:00:00+07:00",
      "summary": "Khách chưa xác nhận lịch demo.", "next_action": "Nhắc khách xác nhận",
      "deadlines": [{"title": "Demo", "due_at": "2026-10-30", "all_day": true, "evidence": "Demo chốt thứ 6 ngày 30/10"}],
      "decisions": [{"text": "Dùng staging mới", "decided_at": "2026-10-19", "evidence": "ok dùng staging mới"}],
      "open_questions": [{"text": "Có cần SSO?", "asked_by": "other", "evidence": "SSO?"}]
    }"#;

    #[test]
    fn valid_answer() {
        let d = parse_digest(VALID, tz()).unwrap();
        assert_eq!(
            (d.status.as_str(), d.ball.as_str()),
            ("at_risk", "customer")
        );
        assert_eq!(d.ball_since.as_deref(), Some("2026-10-20T09:00:00+07:00"));
        assert_eq!(d.next_action.as_deref(), Some("Nhắc khách xác nhận"));
        assert_eq!(
            d.deadlines,
            vec![Deadline {
                title: "Demo".into(),
                due_at: "2026-10-30T00:00:00+07:00".into(),
                all_day: true,
                evidence: "Demo chốt thứ 6 ngày 30/10".into(),
            }]
        );
        assert_eq!(d.decisions[0].decided_at.as_deref(), Some("2026-10-19"));
        assert_eq!(d.open_questions[0].asked_by, "other");
    }

    #[test]
    fn fenced_answer_with_extra_fields_is_accepted() {
        let raw = format!(
            "Here you go:\n```json\n{}\n```",
            VALID.replacen("{", "{\"confidence\": 0.9, \"extra\": {\"a\": 1},", 1)
        );
        let d = parse_digest(&raw, tz()).unwrap();
        assert_eq!(d.status, "at_risk");
    }

    #[test]
    fn invalid_json_and_wrong_shapes_are_errors() {
        assert!(parse_digest("Sorry, I cannot help.", tz())
            .unwrap_err()
            .contains("no JSON"));
        assert!(parse_digest("{\"status\": \"done\",", tz()).is_err());
        assert!(parse_digest("{\"status\": oops}", tz())
            .unwrap_err()
            .starts_with("invalid JSON"));
        let e = parse_digest(&VALID.replace("\"at_risk\"", "\"late\""), tz()).unwrap_err();
        assert!(e.contains("\"status\" must be one of"), "{e}");
        let e = parse_digest(&VALID.replace("\"customer\"", "\"client\""), tz()).unwrap_err();
        assert!(e.contains("\"ball\""), "{e}");
        let e =
            parse_digest(r#"{"status":"done","ball":"none","summary":"  "}"#, tz()).unwrap_err();
        assert!(e.contains("summary"), "{e}");
        let e = parse_digest(
            r#"{"status":"done","ball":"none","summary":"x","deadlines":"soon"}"#,
            tz(),
        )
        .unwrap_err();
        assert!(e.contains("must be an array"), "{e}");
        assert!(parse_digest(r#"{"ball":"none","summary":"x"}"#, tz())
            .unwrap_err()
            .contains("\"status\" is missing"));
    }

    #[test]
    fn items_without_evidence_or_date_are_dropped_and_text_is_capped() {
        let raw = format!(
            r#"{{"status":"on_track","ball":"none","summary":"{long}","next_action":null,
            "deadlines":[
              {{"title":"No evidence","due_at":"2026-11-01"}},
              {{"title":"Bad date","due_at":"next Friday","evidence":"next Friday"}},
              {{"title":"Timed","due_at":"2026-11-02T15:00","evidence":"3pm on 2/11"}},
              {{"title":"Offset","due_at":"2026-11-03T10:00:00Z","evidence":"10:00 UTC"}}
            ],
            "decisions":[{{"text":"No evidence"}},{{"text":"Kept","evidence":"{ev}"}}],
            "open_questions":[{{"text":"Q","asked_by":"robot"}},{{"asked_by":"me"}}]}}"#,
            long = "a".repeat(800),
            ev = "e".repeat(300),
        );
        let d = parse_digest(&raw, tz()).unwrap();
        assert_eq!(d.summary.chars().count(), 500);
        assert_eq!(d.next_action, None);
        let titles: Vec<&str> = d.deadlines.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, vec!["Timed", "Offset"]);
        assert_eq!(d.deadlines[0].due_at, "2026-11-02T15:00:00+07:00");
        assert!(!d.deadlines[0].all_day);
        assert_eq!(d.deadlines[1].due_at, "2026-11-03T10:00:00+00:00");
        assert_eq!(d.decisions.len(), 1);
        assert_eq!(d.decisions[0].evidence.chars().count(), 200);
        assert_eq!(d.open_questions.len(), 1);
        assert_eq!(d.open_questions[0].asked_by, "other");
        assert_eq!(d.open_questions[0].evidence, None);
    }

    #[test]
    fn lists_are_capped_at_ten() {
        let items: Vec<String> = (0..15)
            .map(|i| format!(r#"{{"text":"d{i}","evidence":"q{i}"}}"#))
            .collect();
        let raw = format!(
            r#"{{"status":"on_track","ball":"none","summary":"x","decisions":[{}]}}"#,
            items.join(",")
        );
        assert_eq!(parse_digest(&raw, tz()).unwrap().decisions.len(), 10);
    }

    #[test]
    fn hard_facts_override_the_ai() {
        let mut d = parse_digest(VALID, tz()).unwrap();
        apply_hard_facts(
            &mut d,
            &HardFacts {
                issue_closed: true,
                ball_mine_since: Some("2026-10-21T01:00:00+00:00".into()),
                board_deadline: Some(("Deadline (board)".into(), "2026-11-05".into())),
            },
            tz(),
        );
        assert_eq!(d.status, "done");
        assert_eq!(d.ball, "me");
        assert_eq!(d.ball_since.as_deref(), Some("2026-10-21T01:00:00+00:00"));
        assert_eq!(d.deadlines.len(), 1);
        assert_eq!(d.deadlines[0].title, "Deadline (board)");
        assert_eq!(d.deadlines[0].due_at, "2026-11-05T00:00:00+07:00");
        assert!(d.deadlines[0].all_day);
        assert_eq!(
            d.deadlines[0].evidence,
            "GitHub: Deadline (board) = 2026-11-05"
        );

        // No facts → AI answer untouched.
        let mut d2 = parse_digest(VALID, tz()).unwrap();
        let before = d2.clone();
        apply_hard_facts(&mut d2, &HardFacts::default(), tz());
        assert_eq!(d2, before);
    }

    #[test]
    fn prompt_carries_context_rules_and_content() {
        let now = tz().with_ymd_and_hms(2026, 10, 22, 9, 30, 0).unwrap();
        let ctx = PromptCtx {
            source_type: "slack",
            title: "Deploy \"v2\"",
            me: "Thai (me)",
            now,
            content: "[2026-10-21T01:00:00+00:00] Bob(other): demo 30/10?",
            truncated: true,
        };
        let p = build_prompt(&ctx);
        assert!(p.contains("Current date/time: 2026-10-22T09:30:00+07:00 (Thursday)"));
        assert!(p.contains("The user is: Thai (me)"));
        assert!(p.contains("titled \"Deploy 'v2'\""));
        assert!(p.contains("Vietnamese"));
        assert!(p.contains("ONE JSON object only"));
        assert!(p.contains("nearest such date on or after"));
        assert!(p.contains("Đã dời từ"));
        assert!(p.contains("middle is omitted"));
        assert!(
            p.contains("<<<SOURCE\n[2026-10-21T01:00:00+00:00] Bob(other): demo 30/10?\nSOURCE>>>")
        );
        let r = retry_prompt(&ctx, "\"status\" is missing");
        assert!(r.starts_with(&p));
        assert!(r.contains("rejected: \"status\" is missing"));
        let gh = build_prompt(&PromptCtx {
            source_type: "github_issue",
            truncated: false,
            ..ctx
        });
        assert!(gh.contains("a GitHub issue"));
        assert!(!gh.contains("middle is omitted"));
    }
}
