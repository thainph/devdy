//! Slack mrkdwn helpers (pure): placeholder protection for translation, plain
//! text rendering (mentions → `@Name`), search transcript and the composer
//! prompt text.

use std::collections::HashMap;

const PH_OPEN: char = '⟦';
const PH_CLOSE: char = '⟧';

fn placeholder(n: usize) -> String {
    format!("{PH_OPEN}{n}{PH_CLOSE}")
}

/// Replace every match found by `find` (returns `(start, end)` byte ranges in
/// order, non-overlapping) with a fresh placeholder.
fn mask_with(
    text: &str,
    slots: &mut Vec<String>,
    find: impl Fn(&str, usize) -> Option<(usize, usize)>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some((start, end)) = find(text, pos) {
        out.push_str(&text[pos..start]);
        out.push_str(&placeholder(slots.len()));
        slots.push(text[start..end].to_string());
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// ```` ```…``` ```` (may span lines).
fn find_code_block(text: &str, from: usize) -> Option<(usize, usize)> {
    let start = from + text[from..].find("```")?;
    let close = text[start + 3..].find("```")?;
    Some((start, start + 3 + close + 3))
}

/// `` `…` `` on one line, non-empty.
fn find_inline_code(text: &str, from: usize) -> Option<(usize, usize)> {
    let mut search = from;
    loop {
        let start = search + text[search..].find('`')?;
        let rest = &text[start + 1..];
        match rest.find(['`', '\n']) {
            Some(i) if rest.as_bytes()[i] == b'`' && i > 0 => {
                return Some((start, start + 1 + i + 1));
            }
            Some(i) if rest.as_bytes()[i] == b'`' => search = start + 2,
            Some(_) => search = start + 1,
            None => return None,
        }
    }
}

/// `<…>` on one line, non-empty.
fn find_angle(text: &str, from: usize) -> Option<(usize, usize)> {
    let mut search = from;
    loop {
        let start = search + text[search..].find('<')?;
        let rest = &text[start + 1..];
        match rest.find(['>', '\n', '<']) {
            Some(i) if rest.as_bytes()[i] == b'>' && i > 0 => {
                return Some((start, start + 1 + i + 1));
            }
            Some(_) => search = start + 1,
            None => return None,
        }
    }
}

fn is_emoji_char(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'+' | b'-' | b'\'')
}

/// Length of `:name:` at `i` (bytes), if any.
fn emoji_at(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b':') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() && is_emoji_char(b[j]) {
        j += 1;
    }
    (j > i + 1 && b.get(j) == Some(&b':')).then_some(j + 1 - i)
}

/// `:emoji:` plus an optional directly following `:skin-tone-N:`.
fn find_emoji(text: &str, from: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let mut i = from;
    while i < b.len() {
        if let Some(len) = emoji_at(b, i) {
            let mut end = i + len;
            if let Some(tone) = emoji_at(b, end) {
                if text[end..end + tone].starts_with(":skin-tone-") {
                    end += tone;
                }
            }
            return Some((i, end));
        }
        i += 1;
    }
    None
}

/// Mask code blocks, inline code, every `<…>` and emoji with `⟦n⟧`
/// placeholders (in that order). Returns the masked text and the slots.
pub fn protect(text: &str) -> (String, Vec<String>) {
    let mut slots = Vec::new();
    let s = mask_with(text, &mut slots, find_code_block);
    let s = mask_with(&s, &mut slots, find_inline_code);
    let s = mask_with(&s, &mut slots, find_angle);
    let s = mask_with(&s, &mut slots, find_emoji);
    (s, slots)
}

/// Placeholder numbers in `text`, sorted (a multiset).
pub fn placeholders(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(PH_OPEN) {
        let after = &rest[i + PH_OPEN.len_utf8()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let tail = &after[digits.len()..];
        if !digits.is_empty() && tail.starts_with(PH_CLOSE) {
            if let Ok(n) = digits.parse() {
                out.push(n);
            }
            rest = &tail[PH_CLOSE.len_utf8()..];
        } else {
            rest = after;
        }
    }
    out.sort_unstable();
    out
}

/// True when `masked` holds nothing but placeholders and whitespace.
pub fn only_placeholders(masked: &str) -> bool {
    let mut rest = masked.to_string();
    for n in placeholders(masked) {
        rest = rest.replacen(&placeholder(n), "", 1);
    }
    rest.trim().is_empty()
}

/// Put the protected tokens back. Stray `<` / `>` produced by the model are
/// escaped first (Slack text never contains bare ones).
pub fn restore(translated: &str, slots: &[String]) -> String {
    let escaped = translated.replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped.as_str();
    while let Some(i) = rest.find(PH_OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + PH_OPEN.len_utf8()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let tail = &after[digits.len()..];
        match digits.parse::<usize>().ok().and_then(|n| slots.get(n)) {
            Some(slot) if tail.starts_with(PH_CLOSE) => {
                out.push_str(slot);
                rest = &tail[PH_CLOSE.len_utf8()..];
            }
            _ => {
                out.push(PH_OPEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Names used to render mentions.
#[derive(Default, Clone)]
pub struct Names {
    /// user id → display name
    pub users: HashMap<String, String>,
    /// channel id → name (without '#')
    pub channels: HashMap<String, String>,
}

impl Names {
    pub fn user(&self, id: &str) -> Option<&str> {
        self.users.get(id).map(String::as_str)
    }
}

/// How `<url|label>` is rendered by `to_plain`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// `label`
    Label,
    /// `[label](url)`
    Markdown,
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Render the inside of one `<…>` token.
fn render_angle(inner: &str, names: &Names, links: Links) -> String {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t, Some(l)),
        None => (inner, None),
    };
    if let Some(id) = target.strip_prefix('@') {
        let name = names
            .user(id)
            .map(str::to_string)
            .or_else(|| label.map(|l| l.trim_start_matches('@').to_string()))
            .unwrap_or_else(|| id.to_string());
        return format!("@{}", decode_entities(&name));
    }
    if let Some(id) = target.strip_prefix('#') {
        let name = label
            .map(str::to_string)
            .or_else(|| names.channels.get(id).cloned())
            .unwrap_or_else(|| id.to_string());
        return format!("#{}", decode_entities(&name));
    }
    if let Some(special) = target.strip_prefix('!') {
        if let Some(l) = label {
            let l = decode_entities(l);
            return if special.starts_with("subteam^") && !l.starts_with('@') {
                format!("@{l}")
            } else {
                l
            };
        }
        let word = special.split('^').next().unwrap_or(special);
        return format!("@{}", word);
    }
    let url = decode_entities(target);
    match (label, links) {
        (Some(l), Links::Markdown) => format!("[{}]({})", decode_entities(l), url),
        (Some(l), Links::Label) => decode_entities(l),
        (None, _) => url
            .strip_prefix("mailto:")
            .map(str::to_string)
            .unwrap_or(url),
    }
}

/// Slack mrkdwn → readable text: mentions become `@Name` / `#channel`, links
/// `label` or `[label](url)`, entities decoded; code is kept as is.
pub fn to_plain(text: &str, names: &Names, links: Links) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some((start, end)) = find_angle(text, pos) {
        out.push_str(&decode_entities(&text[pos..start]));
        out.push_str(&render_angle(&text[start + 1..end - 1], names, links));
        pos = end;
    }
    out.push_str(&decode_entities(&text[pos..]));
    out
}

/// User ids mentioned as `<@U…>` in `text`.
pub fn mentioned_users(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((start, end)) = find_angle(text, pos) {
        let inner = &text[start + 1..end - 1];
        if let Some(id) = inner.strip_prefix('@') {
            let id = id.split('|').next().unwrap_or(id);
            if !id.is_empty() && !out.iter().any(|x| x == id) {
                out.push(id.to_string());
            }
        }
        pos = end;
    }
    out
}

/// Channel ids mentioned as `<#C…>` WITHOUT a label.
pub fn unlabeled_channels(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((start, end)) = find_angle(text, pos) {
        let inner = &text[start + 1..end - 1];
        if let Some(id) = inner.strip_prefix('#') {
            if !id.contains('|') && !id.is_empty() && !out.iter().any(|x| x == id) {
                out.push(id.to_string());
            }
        }
        pos = end;
    }
    out
}

/// First non-empty line of the plain text (quote marker stripped), capped.
pub fn first_line(plain: &str, max_chars: usize) -> String {
    plain
        .lines()
        .map(|l| l.trim().trim_start_matches('>').trim())
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(max_chars).collect())
        .unwrap_or_default()
}

/// One message as needed by the transcript / prompt text.
pub struct TextMessage<'a> {
    pub author: &'a str,
    pub ts: &'a str,
    pub text_raw: &'a str,
    pub text_vi: Option<&'a str>,
    /// `📎 …` lines (already formatted, without the prefix).
    pub files: Vec<String>,
}

/// Search transcript: per message `Name: <vi>` and `Name: <original>`.
pub fn transcript(messages: &[TextMessage], names: &Names) -> String {
    let mut out = String::new();
    for m in messages {
        if let Some(vi) = m.text_vi {
            out.push_str(&format!(
                "{}: {}\n",
                m.author,
                to_plain(vi, names, Links::Label)
            ));
        }
        out.push_str(&format!(
            "{}: {}\n",
            m.author,
            to_plain(m.text_raw, names, Links::Label)
        ));
    }
    out
}

/// `1789877283.026359` → UTC datetime.
pub fn ts_to_datetime(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
    let secs: i64 = secs.parse().ok()?;
    let micros: u32 = format!("{:0<6}", frac.chars().take(6).collect::<String>())
        .parse()
        .ok()?;
    chrono::DateTime::from_timestamp(secs, micros * 1000)
}

/// ISO 8601 (UTC, millis) of a Slack ts.
pub fn ts_to_iso(ts: &str) -> Option<String> {
    ts_to_datetime(ts).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Max size of the composer prompt text.
pub const PROMPT_TEXT_MAX_CHARS: usize = 20_000;

/// Thread-level data for `prompt_text`.
pub struct PromptHeader<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub channel: Option<&'a str>,
    pub workspace: Option<&'a str>,
    pub thread_url: Option<&'a str>,
    pub fetch_status: &'a str,
    /// Legacy markdown, used when there are no messages yet.
    pub legacy_content: Option<&'a str>,
}

/// Composer transcript (see SRS §13.9). Returns `(text, truncated)`.
pub fn prompt_text(
    h: &PromptHeader,
    messages: &[TextMessage],
    legacy_files: &[String],
    names: &Names,
    include_original: bool,
    max_chars: usize,
) -> (String, bool) {
    let mut head = format!(
        "[Slack] {} (id: {}) — #{} · {} · {}\n",
        h.title,
        h.id,
        h.channel.unwrap_or("?"),
        h.workspace.unwrap_or("?"),
        h.thread_url.unwrap_or("")
    );
    if h.fetch_status != "ready" {
        head.push_str(&format!("(status: {})\n", h.fetch_status));
    }
    let mut chunks: Vec<String> = Vec::new();
    if messages.is_empty() {
        if let Some(legacy) = h.legacy_content {
            let mut c = legacy.trim_end().to_string();
            c.push('\n');
            for f in legacy_files {
                c.push_str(&format!("  📎 {f}\n"));
            }
            chunks.push(c);
        }
    }
    for m in messages {
        let when = ts_to_datetime(m.ts)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default();
        let body = match m.text_vi {
            Some(vi) => to_plain(vi, names, Links::Markdown),
            None => format!(
                "{} [chưa dịch]",
                to_plain(m.text_raw, names, Links::Markdown)
            ),
        };
        let mut c = format!("{} ({}): {}\n", m.author, when, body);
        if include_original && m.text_vi.is_some() {
            c.push_str(&format!(
                "  > Gốc: {}\n",
                to_plain(m.text_raw, names, Links::Markdown)
            ));
        }
        for f in &m.files {
            c.push_str(&format!("  📎 {f}\n"));
        }
        chunks.push(c);
    }

    let mut out = head;
    let mut used = out.chars().count();
    let mut body_offset = 0usize;
    for chunk in &chunks {
        let len = chunk.chars().count();
        if used + len > max_chars {
            out.push_str(&format!(
                "… (truncated — read the rest with mcp__devdy__slack_threads_read id={} offset={})\n",
                h.id, body_offset
            ));
            return (out, true);
        }
        out.push_str(chunk);
        used += len;
        body_offset += len;
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_restore_round_trip() {
        let raw = "Hi <@U1>, see <https://x|y> :smile: and `a<b` then\n```let x = 1;\n<@U2>```\n:+1::skin-tone-2: done &lt;ok&gt;";
        let (masked, slots) = protect(raw);
        assert_eq!(slots[0], "```let x = 1;\n<@U2>```");
        assert_eq!(slots[1], "`a<b`");
        assert_eq!(slots[2], "<@U1>");
        assert_eq!(slots[3], "<https://x|y>");
        assert_eq!(slots[4], ":smile:");
        assert_eq!(slots[5], ":+1::skin-tone-2:");
        assert_eq!(
            masked,
            "Hi ⟦2⟧, see ⟦3⟧ ⟦4⟧ and ⟦1⟧ then\n⟦0⟧\n⟦5⟧ done &lt;ok&gt;"
        );
        assert_eq!(placeholders(&masked), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(restore(&masked, &slots), raw);
        // A translation that moves placeholders around restores them verbatim.
        let vi = "Chào ⟦2⟧, xem ⟦3⟧ ⟦4⟧ và ⟦1⟧ rồi\n⟦0⟧\n⟦5⟧ xong &lt;ok&gt;";
        assert_eq!(
            restore(vi, &slots),
            "Chào <@U1>, xem <https://x|y> :smile: và `a<b` rồi\n```let x = 1;\n<@U2>```\n:+1::skin-tone-2: xong &lt;ok&gt;"
        );
        // Bare angle brackets from the model are escaped.
        assert_eq!(
            restore("a < b ⟦0⟧", &slots[..1]),
            "a &lt; b ```let x = 1;\n<@U2>```"
        );
        // A missing placeholder is detectable through the multiset.
        assert_ne!(placeholders("Chào ⟦2⟧"), placeholders(&masked));
        // Unknown slot numbers stay literal.
        assert_eq!(restore("x ⟦9⟧", &slots[..1]), "x ⟦9⟧");
    }

    #[test]
    fn placeholder_only_messages() {
        let (m, _) = protect("<@U1> :tada:");
        assert!(only_placeholders(&m));
        let (m, _) = protect("```code only```");
        assert!(only_placeholders(&m));
        let (m, _) = protect("<@U1> thanks");
        assert!(!only_placeholders(&m));
        // A time is not an emoji; an unterminated fence is plain text.
        let (m, s) = protect("at 10:30 ```oops");
        assert!(s.is_empty());
        assert_eq!(m, "at 10:30 ```oops");
    }

    #[test]
    fn plain_rendering() {
        let mut names = Names::default();
        names.users.insert("U1".into(), "Alice".into());
        names.channels.insert("C9".into(), "dev".into());
        let raw = "<@U1> <@U2|bob> <#C9> <#C8|ops> <!here> <!subteam^S1|@devs> <https://a.b/c?x=1&amp;y=2|docs> <https://z.io> <mailto:a@b.c|a@b.c> &lt;tag&gt; &amp;";
        assert_eq!(
            to_plain(raw, &names, Links::Label),
            "@Alice @bob #dev #ops @here @devs docs https://z.io a@b.c <tag> &"
        );
        assert_eq!(
            to_plain("see <https://a.b|docs>", &names, Links::Markdown),
            "see [docs](https://a.b)"
        );
        assert_eq!(mentioned_users(raw), vec!["U1", "U2"]);
        assert_eq!(unlabeled_channels(raw), vec!["C9"]);
        assert_eq!(first_line("\n  > quoted line\nmore", 5), "quote");
    }

    #[test]
    fn ts_conversion() {
        assert_eq!(
            ts_to_iso("1789877283.026359").as_deref(),
            Some("2026-09-20T04:08:03.026Z")
        );
        assert!(ts_to_iso("abc").is_none());
    }

    #[test]
    fn prompt_text_format_and_truncation() {
        let mut names = Names::default();
        names.users.insert("U1".into(), "Alice".into());
        let h = PromptHeader {
            id: "t1",
            title: "Deploy",
            channel: Some("dev"),
            workspace: Some("malme"),
            thread_url: Some("https://malme.slack.com/archives/C1/p1"),
            fetch_status: "ready",
            legacy_content: None,
        };
        let msgs = vec![
            TextMessage {
                author: "Alice",
                ts: "1789877283.026359",
                text_raw: "Hello <@U1> <https://a.b|docs>",
                text_vi: Some("Xin chào <@U1> <https://a.b|docs>"),
                files: vec!["a.png — /data/a.png".into()],
            },
            TextMessage {
                author: "Bob",
                ts: "1789877290.000000",
                text_raw: "untranslated",
                text_vi: None,
                files: vec![],
            },
        ];
        let (text, truncated) = prompt_text(&h, &msgs, &[], &names, true, 20_000);
        assert!(!truncated);
        assert_eq!(
            text,
            "[Slack] Deploy (id: t1) — #dev · malme · https://malme.slack.com/archives/C1/p1\n\
             Alice (2026-09-20 04:08 UTC): Xin chào @Alice [docs](https://a.b)\n\
             \x20 > Gốc: Hello @Alice [docs](https://a.b)\n\
             \x20 📎 a.png — /data/a.png\n\
             Bob (2026-09-20 04:08 UTC): untranslated [chưa dịch]\n"
        );
        let (text, truncated) = prompt_text(&h, &msgs, &[], &names, false, 150);
        assert!(truncated);
        assert!(text.ends_with(
            "… (truncated — read the rest with mcp__devdy__slack_threads_read id=t1 offset=0)\n"
        ));
        assert!(!text.contains("Alice ("));

        let legacy = PromptHeader {
            fetch_status: "pending",
            legacy_content: Some("# Old body"),
            ..h
        };
        let (text, _) = prompt_text(
            &legacy,
            &[],
            &["x.png — /d/x.png".into()],
            &names,
            false,
            20_000,
        );
        assert!(text.contains("(status: pending)\n# Old body\n  📎 x.png — /d/x.png\n"));
    }
}
