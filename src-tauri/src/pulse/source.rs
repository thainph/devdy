//! Source normalisation (BR-02, pure). Mirrored by
//! `sidecar-mcp/lib/pulse-source.mjs`; both sides run `source_vectors.json`.
//!
//! - Slack thread link (via `slack_threads::link`) → `<channel_id>:<thread_ts>`
//!   (a link to a reply resolves to its thread root).
//! - GitHub issue: `https://github.com/<o>/<r>/issues/<n>` or `<o>/<r>#<n>`
//!   → `<o>/<r>#<n>` with owner/repo lowercased; a bare `#<n>` needs the
//!   project to have exactly one GitHub repo. Pull requests are refused.

use crate::slack_threads::link::{self, LinkError};
use crate::slack_threads::CodedError;

pub const INVALID_SOURCE_MESSAGE: &str = "only Slack thread or GitHub issue links are supported";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Slack,
    GithubIssue,
}

impl SourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceType::Slack => "slack",
            SourceType::GithubIssue => "github_issue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub source_type: SourceType,
    pub source_ref: String,
    /// Canonical link to open the source.
    pub source_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// Not a Slack thread / GitHub issue (detail in English).
    Invalid(String),
    /// Bare `#<n>` while the project has 0 or > 1 GitHub repos.
    RepoAmbiguous,
}

impl From<SourceError> for CodedError {
    fn from(e: SourceError) -> Self {
        match e {
            SourceError::Invalid(d) => CodedError::new("invalid_source", d),
            SourceError::RepoAmbiguous => {
                CodedError::new("repo_ambiguous", "write the issue as owner/repo#number")
            }
        }
    }
}

fn invalid() -> SourceError {
    SourceError::Invalid(INVALID_SOURCE_MESSAGE.to_string())
}

pub fn slack_ref(channel_id: &str, thread_ts: &str) -> String {
    format!("{channel_id}:{thread_ts}")
}

pub fn github_ref(owner: &str, repo: &str, number: u64) -> String {
    format!(
        "{}/{}#{}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase(),
        number
    )
}

pub fn github_url(owner: &str, repo: &str, number: u64) -> String {
    format!(
        "https://github.com/{}/{}/issues/{}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase(),
        number
    )
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

/// `[0-9]{1,10}` > 0.
fn issue_number(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok().filter(|n| *n > 0)
}

fn github_source(owner: &str, repo: &str, number: u64) -> Source {
    Source {
        source_type: SourceType::GithubIssue,
        source_ref: github_ref(owner, repo, number),
        source_url: github_url(owner, repo, number),
    }
}

/// `[http(s)://][www.]github.com/<o>/<r>/issues/<n>[/][?…][#…]`.
fn parse_github_url(s: &str) -> Option<Result<Source, SourceError>> {
    let rest = match s.split_once("://") {
        Some((scheme, rest)) => {
            if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
                return None;
            }
            rest
        }
        None => s,
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    let [owner, repo, kind, number] = parts.as_slice() else {
        return Some(Err(invalid()));
    };
    if *kind == "pull" {
        return Some(Err(SourceError::Invalid(
            "pull requests are not supported (use the PR inbox); link a GitHub issue".into(),
        )));
    }
    let n = issue_number(number);
    match n {
        Some(n) if *kind == "issues" && is_owner(owner) && is_repo(repo) => {
            Some(Ok(github_source(owner, repo, n)))
        }
        _ => Some(Err(invalid())),
    }
}

/// `<o>/<r>#<n>` or bare `#<n>` (resolved against `project_repos`, `owner/repo`).
fn parse_github_short(s: &str, project_repos: &[String]) -> Option<Result<Source, SourceError>> {
    let (head, number) = s.rsplit_once('#')?;
    let n = issue_number(number)?;
    if head.is_empty() {
        let mut repos: Vec<String> = project_repos
            .iter()
            .map(|r| r.trim().to_ascii_lowercase())
            .filter(|r| !r.is_empty())
            .collect();
        repos.sort();
        repos.dedup();
        return Some(match repos.as_slice() {
            [only] => match only.split_once('/') {
                Some((o, r)) if is_owner(o) && is_repo(r) => Ok(github_source(o, r, n)),
                _ => Err(SourceError::RepoAmbiguous),
            },
            _ => Err(SourceError::RepoAmbiguous),
        });
    }
    let (owner, repo) = head.split_once('/')?;
    if !is_owner(owner) || !is_repo(repo) {
        return None;
    }
    Some(Ok(github_source(owner, repo, n)))
}

/// Normalise a pasted link / reference. `project_repos` (`owner/repo`) only
/// matters for a bare `#<n>`.
pub fn parse_source(input: &str, project_repos: &[String]) -> Result<Source, SourceError> {
    let mut s = input.trim();
    if let Some(inner) = s.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
        s = inner.trim();
    }
    if s.is_empty() {
        return Err(invalid());
    }
    match link::parse_thread_link(s) {
        Ok(l) => {
            return Ok(Source {
                source_type: SourceType::Slack,
                source_ref: slack_ref(&l.channel_id, &l.thread_ts),
                source_url: l.canonical_url,
            })
        }
        Err(LinkError::DmUnsupported) => {
            return Err(SourceError::Invalid(link::DM_UNSUPPORTED_MESSAGE.into()))
        }
        Err(LinkError::InvalidLink) => {}
    }
    if let Some(r) = parse_github_url(s) {
        return r;
    }
    if let Some(r) = parse_github_short(s, project_repos) {
        return r;
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        input: String,
        #[serde(default)]
        repos: Vec<String>,
        #[serde(rename = "type")]
        source_type: Option<String>,
        #[serde(rename = "ref")]
        source_ref: Option<String>,
        url: Option<String>,
        error: Option<String>,
    }

    /// Shared with `sidecar-mcp/test-pulse.mjs`.
    #[test]
    fn shared_vectors() {
        let vectors: Vec<Vector> =
            serde_json::from_str(include_str!("source_vectors.json")).unwrap();
        assert!(vectors.len() > 20);
        for v in vectors {
            let got = parse_source(&v.input, &v.repos);
            match (&v.error, got) {
                (Some(code), Err(e)) => {
                    assert_eq!(&CodedError::from(e).code, code, "{}", v.input)
                }
                (None, Ok(s)) => {
                    assert_eq!(
                        Some(s.source_type.as_str()),
                        v.source_type.as_deref(),
                        "{}",
                        v.input
                    );
                    assert_eq!(Some(&s.source_ref), v.source_ref.as_ref(), "{}", v.input);
                    assert_eq!(Some(&s.source_url), v.url.as_ref(), "{}", v.input);
                }
                (want, got) => panic!("{}: want {:?}, got {:?}", v.input, want, got),
            }
        }
    }

    #[test]
    fn error_codes() {
        let e: CodedError = parse_source("hello", &[]).unwrap_err().into();
        assert_eq!(e.code, "invalid_source");
        assert_eq!(e.detail, INVALID_SOURCE_MESSAGE);
        let e: CodedError = parse_source("#3", &[]).unwrap_err().into();
        assert_eq!(e.code, "repo_ambiguous");
    }
}
