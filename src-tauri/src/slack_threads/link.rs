//! Slack thread link parsing (pure).
//!
//! Accepts `https://<workspace>.slack.com/archives/<channel>/p<16 digits>`,
//! optionally with `?thread_ts=<root>&cid=<channel>` (a link to a reply).

use super::CodedError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLink {
    /// e.g. "malme.slack.com" (lowercase).
    pub host: String,
    /// e.g. "C0B83GHDTFG".
    pub channel_id: String,
    /// ts in the path: "1789877283.026359".
    pub message_ts: String,
    /// Thread root: query `thread_ts` if present, else `message_ts`.
    pub thread_ts: String,
    /// `https://<host>/archives/<channel_id>/p<thread_ts without dot>`.
    pub canonical_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    InvalidLink,
    DmUnsupported,
}

pub const INVALID_LINK_MESSAGE: &str =
    "invalid Slack thread link (expected https://<workspace>.slack.com/archives/<channel>/p<16 digits>)";
pub const DM_UNSUPPORTED_MESSAGE: &str = "direct-message threads are not supported";

impl LinkError {
    pub fn code(&self) -> &'static str {
        match self {
            LinkError::InvalidLink => "invalid_link",
            LinkError::DmUnsupported => "dm_unsupported",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            LinkError::InvalidLink => INVALID_LINK_MESSAGE,
            LinkError::DmUnsupported => DM_UNSUPPORTED_MESSAGE,
        }
    }
}

impl From<LinkError> for CodedError {
    fn from(e: LinkError) -> Self {
        CodedError::new(e.code(), e.message())
    }
}

/// `https://<host>/archives/<channel>/p<ts without dot>`.
pub fn canonical_url(host: &str, channel_id: &str, thread_ts: &str) -> String {
    format!(
        "https://{}/archives/{}/p{}",
        host,
        channel_id,
        thread_ts.replace('.', "")
    )
}

/// `^[a-z0-9][a-z0-9-]*(\.enterprise)?\.slack\.com$`
fn is_slack_host(host: &str) -> bool {
    let Some(rest) = host.strip_suffix(".slack.com") else {
        return false;
    };
    let label = rest.strip_suffix(".enterprise").unwrap_or(rest);
    let mut chars = label.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `^\d{10}\.\d{6}$`
pub fn is_slack_ts(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 17
        && b[10] == b'.'
        && b[..10].iter().all(u8::is_ascii_digit)
        && b[11..].iter().all(u8::is_ascii_digit)
}

/// Minimal `%XX` decoding for query values (invalid escapes kept verbatim).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a Slack thread link (see the module docs).
pub fn parse_thread_link(input: &str) -> Result<ThreadLink, LinkError> {
    use LinkError::InvalidLink;
    let mut s = input.trim();
    if let Some(inner) = s.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
        s = inner.trim();
    }
    let (scheme, rest) = s.split_once("://").ok_or(InvalidLink)?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(InvalidLink);
    }
    let rest = rest.split('#').next().unwrap_or("");
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let host = rest[..auth_end].to_ascii_lowercase();
    if !is_slack_host(&host) {
        return Err(InvalidLink);
    }
    let tail = &rest[auth_end..];
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));

    // ^/archives/([CGD][A-Z0-9]{8,})/p(\d{16})/?$
    let path = path.strip_suffix('/').unwrap_or(path);
    let after = path.strip_prefix("/archives/").ok_or(InvalidLink)?;
    let (channel, p) = after.split_once('/').ok_or(InvalidLink)?;
    let digits = p.strip_prefix('p').ok_or(InvalidLink)?;
    let channel_ok = channel.len() >= 9
        && matches!(channel.as_bytes()[0], b'C' | b'G' | b'D')
        && channel
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let digits_ok = digits.len() == 16 && digits.bytes().all(|c| c.is_ascii_digit());
    if !channel_ok || !digits_ok {
        return Err(InvalidLink);
    }
    if channel.starts_with('D') {
        return Err(LinkError::DmUnsupported);
    }
    let message_ts = format!("{}.{}", &digits[..10], &digits[10..]);

    let mut thread_ts = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k {
            "thread_ts" if is_slack_ts(&v) => thread_ts = Some(v),
            "cid" if v != channel => return Err(InvalidLink),
            _ => {}
        }
    }
    let thread_ts = thread_ts.unwrap_or_else(|| message_ts.clone());
    Ok(ThreadLink {
        canonical_url: canonical_url(&host, channel, &thread_ts),
        host,
        channel_id: channel.to_string(),
        message_ts,
        thread_ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_table() {
        let l = parse_thread_link("https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359")
            .unwrap();
        assert_eq!(l.host, "malme.slack.com");
        assert_eq!(l.channel_id, "C0B83GHDTFG");
        assert_eq!(l.message_ts, "1789877283.026359");
        assert_eq!(l.thread_ts, "1789877283.026359");
        assert_eq!(
            l.canonical_url,
            "https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359"
        );

        let l = parse_thread_link(
            "https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG",
        )
        .unwrap();
        assert_eq!(l.channel_id, "C0B83GHDTFG");
        assert_eq!(l.message_ts, "1789877300.000100");
        assert_eq!(l.thread_ts, "1789877283.026359");
        assert_eq!(
            l.canonical_url,
            "https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359"
        );

        assert_eq!(
            parse_thread_link(
                "https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C999"
            ),
            Err(LinkError::InvalidLink)
        );
        assert_eq!(
            parse_thread_link("https://malme.slack.com/archives/D01ABCDEF/p1789877283026359"),
            Err(LinkError::DmUnsupported)
        );
    }

    #[test]
    fn rejects_bad_host_missing_p_and_short_ts() {
        for bad in [
            "https://malme.example.com/archives/C0B83GHDTFG/p1789877283026359",
            "https://slack.com/archives/C0B83GHDTFG/p1789877283026359",
            "https://evil.com/malme.slack.com/archives/C0B83GHDTFG/p1789877283026359",
            "https://-x.slack.com/archives/C0B83GHDTFG/p1789877283026359",
            "https://malme.slack.com:8443/archives/C0B83GHDTFG/p1789877283026359",
            "https://malme.slack.com/archives/C0B83GHDTFG/1789877283026359",
            "https://malme.slack.com/archives/C0B83GHDTFG/p178987728302635",
            "https://malme.slack.com/archives/C0B83GHDTFG/p17898772830263590",
            "https://malme.slack.com/archives/C0B83/p1789877283026359",
            "https://malme.slack.com/archives/X0B83GHDTFG/p1789877283026359",
            "https://malme.slack.com/archives/c0b83ghdtfg/p1789877283026359",
            "https://app.slack.com/client/T1/C0B83GHDTFG",
            "ftp://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359",
            "abc",
            "",
        ] {
            assert_eq!(
                parse_thread_link(bad),
                Err(LinkError::InvalidLink),
                "{}",
                bad
            );
        }
    }

    #[test]
    fn normalizes_wrapping_scheme_case_and_extras() {
        let l = parse_thread_link(
            "  <http://Malme.Slack.com/archives/G0B83GHDTFG/p1789877283026359/?foo=1#x>  ",
        )
        .unwrap();
        assert_eq!(l.host, "malme.slack.com");
        assert_eq!(l.channel_id, "G0B83GHDTFG");
        assert_eq!(
            l.canonical_url,
            "https://malme.slack.com/archives/G0B83GHDTFG/p1789877283026359"
        );
        let l = parse_thread_link(
            "https://acme.enterprise.slack.com/archives/C0B83GHDTFG/p1789877283026359?thread_ts=1789877283.026359",
        )
        .unwrap();
        assert_eq!(l.host, "acme.enterprise.slack.com");
        // Encoded dot and an unparsable thread_ts (ignored).
        let l = parse_thread_link(
            "https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283%2E026359",
        )
        .unwrap();
        assert_eq!(l.thread_ts, "1789877283.026359");
        let l = parse_thread_link(
            "https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=abc",
        )
        .unwrap();
        assert_eq!(l.thread_ts, "1789877300.000100");
        assert_eq!(LinkError::DmUnsupported.code(), "dm_unsupported");
        let e: CodedError = LinkError::InvalidLink.into();
        assert_eq!(
            e.to_string(),
            format!("invalid_link: {}", INVALID_LINK_MESSAGE)
        );
    }
}
