//! Todo source normalisation (spec BR-02, pure). Mirrored by
//! `sidecar-mcp/lib/todo-source.mjs`; both sides run `source_vectors.json`.
//!
//! | input                                              | type     | ref                              |
//! |----------------------------------------------------|----------|----------------------------------|
//! | Slack thread link (a reply resolves to its root)   | `slack`  | `slack:<team>/<channel>/<ts>`    |
//! | `github.com/<o>/<r>/issues/<n>` · `/pull/<n>` · `<o>/<r>#<n>` | `github` | `github:<o>/<r>#<n>` (lowercase) |
//! | `<host>/<path>/-/issues/<n>` · `/-/merge_requests/<n>` | `gitlab` | `gitlab:<path>#<n>` · `!<n>`     |
//! | `run:<id>` · `devdy://run/<id>`                    | `run`    | `run:<id>`                       |
//! | any other http(s) URL                              | `url`    | none                             |
//!
//! The URL is kept as given (trimmed, `<…>` unwrapped); anything else is an
//! error. One source may back several todos (refs are not unique).

use crate::slack_threads::link;

pub const INVALID_SOURCE_MESSAGE: &str =
    "invalid source: expected a link (Slack thread, GitHub/GitLab issue or PR, run:<id>, or an http(s) URL)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoSource {
    /// `slack | github | gitlab | run | url`.
    pub source_type: &'static str,
    pub source_ref: Option<String>,
    pub source_url: String,
}

/// `[0-9]{1,10}` > 0.
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok().filter(|n| *n > 0)
}

/// GitHub user / org name: `[A-Za-z0-9-]`, not starting with `-`.
fn is_owner(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('-')
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Repo name: `[A-Za-z0-9._-]`, not `.` / `..`.
fn is_repo(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Run ids are uuids; accept any short token.
fn is_run_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

fn github_ref(owner: &str, repo: &str, n: u64) -> String {
    format!(
        "github:{}/{}#{n}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    )
}

/// `(host, path segments)` of an http(s) URL; query / fragment dropped.
fn http_parts(s: &str) -> Option<(String, Vec<&str>)> {
    let (scheme, rest) = s.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let segs = path.split('/').filter(|p| !p.is_empty()).collect();
    Some((host.to_ascii_lowercase(), segs))
}

fn github_from_url(host: &str, segs: &[&str]) -> Option<String> {
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    match segs {
        [owner, repo, "issues" | "pull", n, ..] if is_owner(owner) && is_repo(repo) => {
            Some(github_ref(owner, repo, number(n)?))
        }
        _ => None,
    }
}

/// `…/<path>/-/issues/<n>` or `…/-/merge_requests/<n>` on any host.
fn gitlab_from_url(segs: &[&str]) -> Option<String> {
    let dash = segs.iter().position(|s| *s == "-")?;
    if dash == 0 {
        return None;
    }
    let path = segs[..dash].join("/").to_ascii_lowercase();
    let sep = match *segs.get(dash + 1)? {
        "issues" => '#',
        "merge_requests" => '!',
        _ => return None,
    };
    let n = number(segs.get(dash + 2)?)?;
    Some(format!("gitlab:{path}{sep}{n}"))
}

/// Normalise a todo's source link. `Ok(None)` for an empty input.
pub fn normalize(input: &str) -> Result<Option<TodoSource>, String> {
    let mut s = input.trim();
    if let Some(inner) = s.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
        s = inner.trim();
    }
    if s.is_empty() {
        return Ok(None);
    }
    let url = s.to_string();
    let found = |source_type: &'static str, source_ref: Option<String>| {
        Ok(Some(TodoSource {
            source_type,
            source_ref,
            source_url: url.clone(),
        }))
    };

    let run_id = s
        .strip_prefix("run:")
        .or_else(|| s.strip_prefix("devdy://run/"))
        .or_else(|| s.strip_prefix("devdy://runs/"))
        .map(|r| r.trim_end_matches('/'));
    if let Some(id) = run_id {
        return if is_run_id(id) {
            found("run", Some(format!("run:{id}")))
        } else {
            Err(INVALID_SOURCE_MESSAGE.to_string())
        };
    }
    if let Ok(l) = link::parse_thread_link(s) {
        let team = l.host.strip_suffix(".slack.com").unwrap_or(&l.host);
        return found(
            "slack",
            Some(format!("slack:{team}/{}/{}", l.channel_id, l.thread_ts)),
        );
    }
    if let Some((host, segs)) = http_parts(s) {
        if let Some(r) = github_from_url(&host, &segs) {
            return found("github", Some(r));
        }
        if let Some(r) = gitlab_from_url(&segs) {
            return found("gitlab", Some(r));
        }
        return found("url", None);
    }
    // Short `<owner>/<repo>#<n>`.
    if let Some((head, n)) = s.rsplit_once('#') {
        if let (Some((owner, repo)), Some(n)) = (head.split_once('/'), number(n)) {
            if is_owner(owner) && is_repo(repo) {
                return found("github", Some(github_ref(owner, repo, n)));
            }
        }
    }
    Err(INVALID_SOURCE_MESSAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        input: String,
        #[serde(rename = "type")]
        source_type: Option<String>,
        #[serde(rename = "ref")]
        source_ref: Option<String>,
        url: Option<String>,
        #[serde(default)]
        error: bool,
    }

    /// Shared with `sidecar-mcp/test-todos.mjs`.
    #[test]
    fn shared_vectors() {
        let vectors: Vec<Vector> =
            serde_json::from_str(include_str!("source_vectors.json")).unwrap();
        assert!(vectors.len() > 15);
        for v in vectors {
            let got = normalize(&v.input);
            if v.error {
                assert_eq!(got, Err(INVALID_SOURCE_MESSAGE.to_string()), "{}", v.input);
                continue;
            }
            match (v.source_type.as_deref(), got.unwrap()) {
                (None, None) => {}
                (Some(t), Some(s)) => {
                    assert_eq!(s.source_type, t, "{}", v.input);
                    assert_eq!(s.source_ref, v.source_ref, "{}", v.input);
                    assert_eq!(Some(&s.source_url), v.url.as_ref(), "{}", v.input);
                }
                (want, got) => panic!("{}: want {want:?}, got {got:?}", v.input),
            }
        }
    }
}
