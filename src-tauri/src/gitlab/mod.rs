//! GitLab REST API v4 client for fetching Issues and Merge Requests.
//!
//! Mirrors the GitHub fetch flow (`commands/github.rs`) but talks to GitLab's
//! REST API using the `PRIVATE-TOKEN` header (SEC-003). Markdown output uses the
//! exact same frontmatter keys and section headers as GitHub so the UI parses
//! both providers identically (BR-004).
//!
//! SECURITY: the PAT is only ever sent as a request header. It is never logged,
//! written to markdown, or embedded in an error message (SEC-002).

use serde_json::Value;

const USER_AGENT: &str = "devdy/0.1";

/// Everything needed to talk to one GitLab project's REST API.
pub struct GitlabClient {
    client: reqwest::Client,
    /// Normalized host, e.g. `https://gitlab.com` (no trailing slash).
    api_base: String,
    /// URL path segment for `:id` — numeric id preferred, else URL-encoded path.
    project_ref: String,
    pat: String,
}

impl GitlabClient {
    /// Build a client for a GitLab project.
    ///
    /// `host` is the account host (already or not-yet normalized); it is
    /// normalized here so callers can pass the raw DB value. `:id` prefers the
    /// numeric `project_id`; otherwise the `namespace/project` path is
    /// URL-encoded (INT-004).
    pub fn new(
        host: Option<&str>,
        project_id: Option<i64>,
        project_path: Option<&str>,
        pat: String,
    ) -> Result<Self, String> {
        let api_base = format!("{}/api/v4", normalize_host(host));
        let project_ref = match project_id {
            Some(id) => id.to_string(),
            None => {
                let path = project_path
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or("Repo GitLab thiếu định danh project (numeric id hoặc path).")?;
                encode_project_path(path)
            }
        };
        Ok(Self {
            client: reqwest::Client::new(),
            api_base,
            project_ref,
            pat,
        })
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/projects/{}/{}",
            self.api_base, self.project_ref, suffix
        )
    }

    /// GET a single JSON value, mapping HTTP status to a Vietnamese error (BR-005).
    async fn get_json(&self, url: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(url)
            .header("PRIVATE-TOKEN", self.pat.trim())
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|_| "Lỗi kết nối tới GitLab. Kiểm tra mạng và host.".to_string())?;

        let status = resp.status();
        if !status.is_success() {
            return Err(map_status_error(status.as_u16()));
        }
        resp.json::<Value>()
            .await
            .map_err(|_| "GitLab trả về dữ liệu không hợp lệ.".to_string())
    }

    /// GET a paginated JSON array, following `X-Next-Page` until exhausted (INT-005).
    async fn get_json_paged(&self, url: &str) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut page: u32 = 1;
        loop {
            let paged_url = if url.contains('?') {
                format!("{}&per_page=100&page={}", url, page)
            } else {
                format!("{}?per_page=100&page={}", url, page)
            };
            let resp = self
                .client
                .get(&paged_url)
                .header("PRIVATE-TOKEN", self.pat.trim())
                .header("User-Agent", USER_AGENT)
                .send()
                .await
                .map_err(|_| "Lỗi kết nối tới GitLab. Kiểm tra mạng và host.".to_string())?;

            let status = resp.status();
            if !status.is_success() {
                return Err(map_status_error(status.as_u16()));
            }
            let next_page = resp
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u32>().ok());

            let body: Value = resp
                .json()
                .await
                .map_err(|_| "GitLab trả về dữ liệu không hợp lệ.".to_string())?;
            if let Some(arr) = body.as_array() {
                out.extend(arr.iter().cloned());
            }

            match next_page {
                Some(n) if n > page => page = n,
                _ => break,
            }
        }
        Ok(out)
    }
}

/// Normalize a GitLab host, falling back to gitlab.com when empty (BR-002).
/// Trailing slashes are stripped so `{host}/api/v4/...` builds cleanly.
pub fn normalize_host(host: Option<&str>) -> String {
    let h = host.map(|s| s.trim()).unwrap_or("");
    let h = if h.is_empty() { "https://gitlab.com" } else { h };
    h.trim_end_matches('/').to_string()
}

/// URL-encode a `namespace/project` path for use as GitLab's `:id` segment.
/// GitLab expects the `/` between namespace and project to be percent-encoded.
fn encode_project_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.trim().bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Map a GitLab HTTP status to a Vietnamese, PAT-safe error message (BR-005).
fn map_status_error(status: u16) -> String {
    match status {
        401 => "GitLab API 401 Unauthorized. Kiểm tra: token còn hạn/đúng, có scope \
                `read_api` (hoặc `api`), và host đúng."
            .to_string(),
        403 => "GitLab API 403 Forbidden. Token thiếu quyền truy cập project này.".to_string(),
        404 => "Không tìm thấy issue/MR trên GitLab (404). Kiểm tra IID và project.".to_string(),
        other => format!("GitLab API lỗi: {other}."),
    }
}

/// Returns true if a GitLab note is a system note (BR-003a): auto-generated
/// activity like label/assignee changes, which must never appear in markdown.
fn is_system_note(note: &Value) -> bool {
    note.get("system").and_then(Value::as_bool).unwrap_or(false)
}

/// Returns true when a note's author should be treated as a bot (BR-003b),
/// reusing the same rules as GitHub's `is_bot_user`.
fn is_bot_author(note: &Value) -> bool {
    let author = note.get("author");
    let username = author
        .and_then(|a| a.get("username"))
        .and_then(Value::as_str)
        .unwrap_or("");
    // GitLab marks service accounts with `bot: true` on the user object.
    let is_bot_flag = author
        .and_then(|a| a.get("bot"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    is_bot_flag || is_bot_user(username)
}

/// Bot detection reused from the GitHub rules (BR-003): `[bot]` suffix or a
/// well-known review-bot username. GitLab has no per-account `type` field like
/// GitHub, so we match on username only (plus the `bot` flag, handled above).
fn is_bot_user(login: &str) -> bool {
    let lower = login.to_ascii_lowercase();
    if lower.ends_with("[bot]") {
        return true;
    }
    matches!(
        lower.as_str(),
        "coderabbitai"
            | "claude"
            | "claude-bot"
            | "github-actions"
            | "dependabot"
            | "renovate"
            | "codecov"
            | "sonarcloud"
            | "greptileai"
            | "sweep-ai"
    )
}

/// A note should be rendered only if it is neither a system note nor from a bot.
fn note_is_human(note: &Value) -> bool {
    !is_system_note(note) && !is_bot_author(note)
}

/// Pick the linked issue IID from a `/closes_issues` response: the smallest IID
/// (BR-006). Returns `None` when there are no closed issues.
fn lowest_closed_issue_iid(closes: &[Value]) -> Option<u64> {
    closes
        .iter()
        .filter_map(|i| i.get("iid").and_then(Value::as_u64))
        .min()
}

/// Build the `repo_slug` used for the task directory (BR-008):
/// `<provider>-<owner_or_namespace>-<repo>-<repo_id[:6]>`, lowercased, every
/// char outside `[a-z0-9]` replaced by `-`, and consecutive `-` collapsed.
pub fn repo_slug(provider: &str, owner: &str, repo: &str, repo_id: &str) -> String {
    let id_suffix: String = repo_id.chars().take(6).collect();
    let raw = format!("{}-{}-{}-{}", provider, owner, repo, id_suffix);
    sanitize_slug(&raw)
}

/// Where a repo's fetched task files live, and how they are named:
///
/// ```text
/// <project>/.devdy/tasks/<repo_slug>/issue-<n>/issue.md
/// <project>/.devdy/tasks/<repo_slug>/pr-<n>/pr.md      (GitHub)
/// <project>/.devdy/tasks/<repo_slug>/mr-<n>/mr.md      (GitLab)
/// ```
///
/// The path is a pure function of the immutable identity `(repo, kind, number)`
/// and deliberately NOT of the linked issue. An earlier layout nested a PR under
/// `issue-<linked>/`, which meant the same PR landed in a different folder
/// depending on whether the link was auto-detected, typed in by hand, or absent
/// (`no-issue/`) — so a re-fetch could fork a second copy instead of overwriting
/// the first. Keying on identity alone makes re-fetch idempotent by construction.
#[derive(Clone, Copy)]
pub struct TaskLocation<'a> {
    /// Absolute path of the project checkout.
    pub project_path: &'a str,
    /// `repo_slug` of the repo these tasks belong to.
    pub repo_slug: &'a str,
    /// `"github"` or `"gitlab"` — only decides `pr-` vs `mr-` naming.
    pub provider: &'a str,
}

impl TaskLocation<'_> {
    /// Canonical file for one issue/PR/MR.
    pub fn file(&self, run_type: &str, number: u64) -> std::path::PathBuf {
        let (dir, file) = if run_type == "analyze_issue" {
            (format!("issue-{}", number), "issue.md")
        } else if self.provider == "gitlab" {
            (format!("mr-{}", number), "mr.md")
        } else {
            (format!("pr-{}", number), "pr.md")
        };
        std::path::Path::new(self.project_path)
            .join(".devdy")
            .join("tasks")
            .join(self.repo_slug)
            .join(dir)
            .join(file)
    }

    /// Relative path from a PR/MR task file to its linked issue's task file,
    /// used as the `linked_issue_file` frontmatter value. Returns `None` unless
    /// the issue file actually exists, so the reference never dangles.
    pub fn linked_issue_reference(&self, issue_number: u64) -> Option<String> {
        self.file("analyze_issue", issue_number)
            .exists()
            .then(|| format!("../issue-{}/issue.md", issue_number))
    }
}

fn sanitize_slug(raw: &str) -> String {
    let lowered = raw.to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_dash = false;
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    // Trim leading/trailing dashes for tidiness.
    out.trim_matches('-').to_string()
}

fn author_name(node: &Value) -> String {
    node.get("author")
        .and_then(|a| a.get("username"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn note_author(node: &Value) -> String {
    node.get("author")
        .and_then(|a| a.get("username"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

impl GitlabClient {
    /// Fetch a GitLab issue + its human notes and render task markdown (FR-002).
    pub async fn build_issue_markdown(&self, issue_iid: u64) -> Result<String, String> {
        let issue = self.get_json(&self.url(&format!("issues/{}", issue_iid))).await?;

        let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
        let author = author_name(&issue);
        let created = issue
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("");
        let labels = issue
            .get("labels")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let body = issue.get("description").and_then(Value::as_str).unwrap_or("");

        let mut md = format!(
            "---\nissue: {}\ntitle: {}\nauthor: {}\ncreated: {}\nlabels: {}\nfetched_at: {}\n---\n\n# {}\n\n{}\n\n",
            issue_iid,
            title,
            author,
            created,
            labels,
            chrono::Utc::now().to_rfc3339(),
            title,
            body,
        );

        let notes = self
            .get_json_paged(&self.url(&format!("issues/{}/notes", issue_iid)))
            .await?;
        for note in notes.iter().filter(|n| note_is_human(n)) {
            let created = note.get("created_at").and_then(Value::as_str).unwrap_or("");
            let body = note.get("body").and_then(Value::as_str).unwrap_or("");
            md.push_str(&format!(
                "---\n\n**Comment by {}** ({})\n\n{}\n\n",
                note_author(note),
                created,
                body,
            ));
        }

        Ok(md)
    }

    /// Resolve the linked issue for an MR: explicit param, else the smallest
    /// `closes_issues` IID, else `NO_LINKED_ISSUE` (BR-006).
    async fn resolve_linked_issue(
        &self,
        mr_iid: u64,
        linked_issue: Option<u64>,
        allow_missing_issue: bool,
    ) -> Result<Option<u64>, String> {
        if let Some(n) = linked_issue {
            return Ok(Some(n));
        }
        let closes = self
            .get_json_paged(&self.url(&format!("merge_requests/{}/closes_issues", mr_iid)))
            .await
            .unwrap_or_default();
        match lowest_closed_issue_iid(&closes) {
            Some(n) => Ok(Some(n)),
            None if allow_missing_issue => Ok(None),
            None => Err("NO_LINKED_ISSUE".to_string()),
        }
    }

    /// Fetch an MR (metadata, changes, notes, approvals) and render task
    /// markdown (FR-003). Returns the markdown and the resolved linked issue.
    pub async fn build_mr_markdown(
        &self,
        mr_iid: u64,
        linked_issue: Option<u64>,
        allow_missing_issue: bool,
        loc: TaskLocation<'_>,
    ) -> Result<String, String> {
        let mr = self
            .get_json(&self.url(&format!("merge_requests/{}", mr_iid)))
            .await?;

        let linked = self
            .resolve_linked_issue(mr_iid, linked_issue, allow_missing_issue)
            .await?;

        // Grouping by issue now lives in the frontmatter instead of the folder
        // layout, so materialize the linked issue's own task file (once) and
        // reference it. Fail-soft: if the issue can't be fetched the reference
        // is simply omitted.
        let linked_file = match linked {
            Some(n) => {
                let path = loc.file("analyze_issue", n);
                if !path.exists() {
                    if let Ok(issue_md) = self.build_issue_markdown(n).await {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&path, issue_md);
                    }
                }
                loc.linked_issue_reference(n)
            }
            None => None,
        };

        let head_sha = mr
            .get("diff_refs")
            .and_then(|r| r.get("head_sha"))
            .or_else(|| mr.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let title = mr.get("title").and_then(Value::as_str).unwrap_or("");
        let author = author_name(&mr);
        let target = mr
            .get("target_branch")
            .and_then(Value::as_str)
            .unwrap_or("");
        let source = mr
            .get("source_branch")
            .and_then(Value::as_str)
            .unwrap_or("");
        let created = mr.get("created_at").and_then(Value::as_str).unwrap_or("");
        let body = mr.get("description").and_then(Value::as_str).unwrap_or("");

        let mut md = format!(
            "---\npr: {}\nlinked_issue: {}\nlinked_issue_file: {}\ntitle: {}\nauthor: {}\nbase: {}\nhead: {}\nhead_sha: {}\ncreated: {}\nfetched_at: {}\n---\n\n# {}\n\n{}\n\n",
            mr_iid,
            linked.map(|n| n.to_string()).unwrap_or_else(|| "none".to_string()),
            linked_file.as_deref().unwrap_or("none"),
            title,
            author,
            target,
            source,
            head_sha,
            created,
            chrono::Utc::now().to_rfc3339(),
            title,
            body,
        );

        // Files changed + diffs (from /changes).
        match self
            .get_json(&self.url(&format!("merge_requests/{}/changes", mr_iid)))
            .await
        {
            Ok(changes) => {
                let files = changes
                    .get("changes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                md.push_str("## Files Changed\n\n");
                for f in &files {
                    let new_path = f.get("new_path").and_then(Value::as_str).unwrap_or("");
                    let status = if f.get("new_file").and_then(Value::as_bool).unwrap_or(false) {
                        "added"
                    } else if f.get("deleted_file").and_then(Value::as_bool).unwrap_or(false) {
                        "deleted"
                    } else if f.get("renamed_file").and_then(Value::as_bool).unwrap_or(false) {
                        "renamed"
                    } else {
                        "modified"
                    };
                    md.push_str(&format!("- `{}` [{}]\n", new_path, status));
                }
                md.push('\n');

                md.push_str("## Diffs\n\n");
                for f in &files {
                    let new_path = f.get("new_path").and_then(Value::as_str).unwrap_or("");
                    if let Some(diff) = f.get("diff").and_then(Value::as_str) {
                        if !diff.is_empty() {
                            md.push_str(&format!(
                                "### `{}`\n\n```diff\n{}\n```\n\n",
                                new_path, diff
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                md.push_str(&format!("## Files Changed\n\n(Lỗi lấy changes: {})\n\n", e));
            }
        }

        // Notes: split into general comments and inline (diff-anchored) notes.
        let notes = self
            .get_json_paged(&self.url(&format!("merge_requests/{}/notes", mr_iid)))
            .await
            .unwrap_or_default();

        let human: Vec<&Value> = notes.iter().filter(|n| note_is_human(n)).collect();

        let general: Vec<&&Value> = human.iter().filter(|n| n.get("position").is_none()).collect();
        if !general.is_empty() {
            md.push_str("## Comments\n\n");
            for note in &general {
                md.push_str(&format!(
                    "**{}** ({}): {}\n\n",
                    note_author(note),
                    note.get("created_at").and_then(Value::as_str).unwrap_or(""),
                    note.get("body").and_then(Value::as_str).unwrap_or(""),
                ));
            }
        }

        let inline: Vec<&&Value> = human.iter().filter(|n| n.get("position").is_some()).collect();
        if !inline.is_empty() {
            md.push_str("## Inline Review Comments\n\n");
            for note in &inline {
                let position = note.get("position");
                let path = position
                    .and_then(|p| p.get("new_path").or_else(|| p.get("old_path")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let line = position
                    .and_then(|p| p.get("new_line").or_else(|| p.get("old_line")))
                    .and_then(Value::as_u64);
                let location = match line {
                    Some(n) => format!("`{}:{}`", path, n),
                    None => format!("`{}`", path),
                };
                md.push_str(&format!(
                    "**{}** on {} ({}):\n\n{}\n\n",
                    note_author(note),
                    location,
                    note.get("created_at").and_then(Value::as_str).unwrap_or(""),
                    note.get("body").and_then(Value::as_str).unwrap_or("").trim(),
                ));
            }
        }

        // Approvals -> ## Reviews (BR-010).
        if let Ok(approvals) = self
            .get_json(&self.url(&format!("merge_requests/{}/approvals", mr_iid)))
            .await
        {
            let approved_by = approvals
                .get("approved_by")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut rendered = String::new();
            for entry in &approved_by {
                let username = entry
                    .get("user")
                    .and_then(|u| u.get("username"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if username.is_empty() || is_bot_user(username) {
                    continue;
                }
                rendered.push_str(&format!("**{}** approved these changes\n\n", username));
            }
            if !rendered.is_empty() {
                md.push_str("## Reviews\n\n");
                md.push_str(&rendered);
            }
        }

        Ok(md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repo_slug_sanitizes_and_suffixes() {
        // Namespace with nested path, dots, spaces, uppercase -> all normalized.
        let slug = repo_slug("gitlab", "My Group/Sub.Group", "Web App", "abcdef0123456789");
        assert_eq!(slug, "gitlab-my-group-sub-group-web-app-abcdef");
    }

    #[test]
    fn repo_slug_collapses_consecutive_dashes() {
        let slug = repo_slug("github", "a__b", "c!!d", "112233");
        assert_eq!(slug, "github-a-b-c-d-112233");
        // No leading/trailing/double dashes remain.
        assert!(!slug.contains("--"));
        assert!(!slug.starts_with('-') && !slug.ends_with('-'));
    }

    #[test]
    fn repo_slug_id_suffix_is_six_chars() {
        let slug = repo_slug("gitlab", "ns", "proj", "0123456789abcdef");
        assert!(slug.ends_with("-012345"));
    }

    fn loc(provider: &'static str) -> TaskLocation<'static> {
        TaskLocation { project_path: "/p", repo_slug: "slug", provider }
    }

    #[test]
    fn task_paths_are_named_per_provider_and_kind() {
        assert_eq!(
            loc("github").file("analyze_issue", 5),
            std::path::PathBuf::from("/p/.devdy/tasks/slug/issue-5/issue.md")
        );
        assert_eq!(
            loc("github").file("review_pr", 5),
            std::path::PathBuf::from("/p/.devdy/tasks/slug/pr-5/pr.md")
        );
        assert_eq!(
            loc("gitlab").file("review_pr", 5),
            std::path::PathBuf::from("/p/.devdy/tasks/slug/mr-5/mr.md")
        );
    }

    #[test]
    fn issue_and_pr_of_the_same_number_do_not_collide() {
        // GitHub shares one number space between issues and PRs, so #5 can be
        // both — the two must never resolve to the same file.
        assert_ne!(
            loc("github").file("analyze_issue", 5),
            loc("github").file("review_pr", 5)
        );
    }

    #[test]
    fn pr_path_ignores_the_linked_issue() {
        // The whole point of the canonical layout: nothing outside
        // (repo, kind, number) can move a PR's file, so a re-fetch that resolves
        // a different linked issue still overwrites the same path.
        let first = loc("github").file("review_pr", 10);
        let second = loc("github").file("review_pr", 10);
        assert_eq!(first, second);
        assert!(!first.to_string_lossy().contains("issue-"));
        assert!(!first.to_string_lossy().contains("no-issue"));
    }

    #[test]
    fn linked_issue_reference_is_none_when_file_missing() {
        // A dangling `linked_issue_file` would send the agent at a path that
        // isn't there, so the reference is omitted instead.
        assert_eq!(loc("github").linked_issue_reference(7), None);
    }

    #[test]
    fn linked_issue_reference_resolves_relative_to_the_pr_file() {
        let dir = std::env::temp_dir().join(format!("devdy-loc-{}", std::process::id()));
        let l = TaskLocation {
            project_path: dir.to_str().unwrap(),
            repo_slug: "slug",
            provider: "github",
        };
        let issue = l.file("analyze_issue", 7);
        std::fs::create_dir_all(issue.parent().unwrap()).unwrap();
        std::fs::write(&issue, "x").unwrap();

        let reference = l.linked_issue_reference(7).expect("issue file exists");
        assert_eq!(reference, "../issue-7/issue.md");

        // The reference must resolve from the PR file's own directory. That dir
        // exists by the time anything reads the reference (the caller creates it
        // to write `pr.md`), and `..` only traverses through real directories.
        let pr_file = l.file("review_pr", 10);
        let pr_dir = pr_file.parent().unwrap();
        std::fs::create_dir_all(pr_dir).unwrap();
        let resolved = pr_dir.join(&reference);
        assert!(resolved.exists(), "{:?} should exist", resolved);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repo_slug_collision_distinct_by_id_suffix() {
        // Same provider/owner/repo but different repo_id -> different slugs.
        let a = repo_slug("gitlab", "ns", "proj", "aaaaaa1111");
        let b = repo_slug("gitlab", "ns", "proj", "bbbbbb2222");
        assert_ne!(a, b);
        assert_eq!(a, "gitlab-ns-proj-aaaaaa");
        assert_eq!(b, "gitlab-ns-proj-bbbbbb");
    }

    #[test]
    fn lowest_closed_issue_picks_smallest_iid() {
        let closes = vec![json!({"iid": 9}), json!({"iid": 3}), json!({"iid": 7})];
        assert_eq!(lowest_closed_issue_iid(&closes), Some(3));
    }

    #[test]
    fn lowest_closed_issue_empty_is_none() {
        let closes: Vec<Value> = vec![];
        assert_eq!(lowest_closed_issue_iid(&closes), None);
    }

    #[test]
    fn system_notes_are_filtered() {
        let sys = json!({"system": true, "body": "changed the label", "author": {"username": "alice"}});
        let human = json!({"system": false, "body": "real comment", "author": {"username": "alice"}});
        assert!(!note_is_human(&sys));
        assert!(note_is_human(&human));
    }

    #[test]
    fn bot_notes_are_filtered() {
        let bot_flag = json!({"system": false, "body": "x", "author": {"username": "svc", "bot": true}});
        let bot_name = json!({"system": false, "body": "x", "author": {"username": "dependabot"}});
        let bot_suffix = json!({"system": false, "body": "x", "author": {"username": "foo[bot]"}});
        let human = json!({"system": false, "body": "x", "author": {"username": "carol"}});
        assert!(!note_is_human(&bot_flag));
        assert!(!note_is_human(&bot_name));
        assert!(!note_is_human(&bot_suffix));
        assert!(note_is_human(&human));
    }

    #[test]
    fn status_errors_map_to_vietnamese() {
        assert!(map_status_error(401).contains("401"));
        assert!(map_status_error(404).contains("Không tìm thấy"));
        assert!(map_status_error(500).contains("500"));
        // PAT never leaks in an error string.
        assert!(!map_status_error(401).to_lowercase().contains("token=") );
    }

    #[test]
    fn normalize_host_defaults_and_trims() {
        assert_eq!(normalize_host(None), "https://gitlab.com");
        assert_eq!(normalize_host(Some("  ")), "https://gitlab.com");
        assert_eq!(normalize_host(Some("https://git.acme.io/")), "https://git.acme.io");
    }

    #[test]
    fn encode_project_path_encodes_slash() {
        assert_eq!(encode_project_path("group/project"), "group%2Fproject");
        assert_eq!(encode_project_path("a.b_c-d"), "a.b_c-d");
    }
}
