//! Pure policy engine for the credential broker (GĐ2, AC1/AC5).
//!
//! This module is deliberately **I/O-free**: no `Db`, no socket, no tokio.
//! `evaluate_policy` is a synchronous pure function so it can be unit-tested
//! without any runtime, which is the core requirement of AC5.
//!
//! Decision priority (see plan §4.2):
//!   1. Denylist  -> Deny   (credential-printing / auth-mutating commands)
//!   2. Read allowlist -> Allow (safe read-only subcommands)
//!   3. Ask list  -> Ask    (write ops + api/graphql backdoors)
//!   4. Default   -> Ask    (any unknown / unlisted subcommand; NEVER silent-Allow)

/// Outcome of policy evaluation for a single `gh`/`glab`/`git` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },
}

/// Split argv into positional tokens (dropping leading-dash flags) plus a flag
/// set for lookups like `--show-token`.
///
/// Positionals (subcommand/group/action) are lowercased so denylist/allowlist
/// matching is case-insensitive: `gh AUTH TOKEN` must resolve to the same Deny
/// as `gh auth token`. Flags are kept verbatim to preserve exact-flag semantics
/// such as `--show-token`.
fn split_argv(argv: &[String]) -> (Vec<String>, Vec<&str>) {
    let mut positionals = Vec::new();
    let mut flags = Vec::new();
    for a in argv {
        if a.starts_with('-') {
            flags.push(a.as_str());
        } else {
            positionals.push(a.to_ascii_lowercase());
        }
    }
    (positionals, flags)
}

fn deny(reason: &str) -> PolicyDecision {
    PolicyDecision::Deny { reason: reason.to_string() }
}

fn ask(reason: &str) -> PolicyDecision {
    PolicyDecision::Ask { reason: reason.to_string() }
}

/// Evaluate the broker policy for a tool + argv. Pure, synchronous, no I/O.
///
/// `tool` is one of `gh`, `glab`, `git` (also accepts `github`/`gitlab` aliases
/// for defensiveness). `argv` is the argument vector AFTER the tool name.
pub fn evaluate_policy(tool: &str, argv: &[String]) -> PolicyDecision {
    match tool {
        "git" => evaluate_git(argv),
        "gh" | "github" => evaluate_gh_like(argv, false),
        "glab" | "gitlab" => evaluate_gh_like(argv, true),
        _ => ask("unknown tool — needs confirmation"),
    }
}

/// git: subcommand is the first positional token.
fn evaluate_git(argv: &[String]) -> PolicyDecision {
    let (pos, _flags) = split_argv(argv);
    let sub = match pos.first() {
        Some(s) => s.as_str(),
        None => return ask("empty git command — needs confirmation"),
    };
    match sub {
        // Read-only / safe transport.
        "fetch" | "pull" | "clone" | "status" | "log" | "diff" => PolicyDecision::Allow,
        // Credential helper protocol (`git credential get/store/erase`). Allowed at
        // the policy layer so the helper is never gated by a modal mid-`git fetch`;
        // the REAL security gate is the host allowlist in `token::resolve_git`.
        "credential" => PolicyDecision::Allow,
        // Write transport — needs confirmation.
        "push" => ask("git push writes to remote — needs confirmation"),
        _ => ask("unknown or unlisted git subcommand — needs confirmation"),
    }
}

/// gh / glab share the same (group, action) shape. `is_glab` only tweaks a few
/// group aliases (`mr` vs `pr`, `project` vs `repo`).
fn evaluate_gh_like(argv: &[String], is_glab: bool) -> PolicyDecision {
    let (pos, flags) = split_argv(argv);
    let group = match pos.first() {
        Some(g) => g.as_str(),
        None => return ask("empty command — needs confirmation"),
    };
    let action = pos.get(1).map(|s| s.as_str());
    let has_show_token = flags.iter().any(|f| *f == "--show-token");

    // ---- 1. DENYLIST (highest priority) ----
    // Credential-printing / auth-mutating / secret / key management.
    if group == "auth" {
        match action {
            Some("token") => return deny("`auth token` prints the credential"),
            Some("setup-git") => return deny("`auth setup-git` writes a global credential helper"),
            Some("login") => return deny("`auth login` mutates auth state"),
            Some("logout") => return deny("`auth logout` mutates auth state"),
            Some("status") if has_show_token => {
                return deny("`auth status --show-token` prints the credential")
            }
            // Bare `auth status` (and any other auth action) is sensitive: fall
            // through to default Ask (NOT allowed as a read).
            _ => return ask("auth subcommand is sensitive — needs confirmation"),
        }
    }
    // `secret` and `ssh-key` groups: fully denied (gh). For glab, `variable`
    // and `ssh-key` are the credential-adjacent equivalents (plan §4.3).
    if group == "secret" || group == "ssh-key" {
        return deny("secret/ssh-key management is blocked");
    }
    if is_glab && group == "variable" {
        return deny("variable management is blocked");
    }

    // ---- 2. READ ALLOWLIST ----
    let is_read = if is_glab {
        matches!(
            (group, action),
            ("mr", Some("list")) | ("mr", Some("view"))
                | ("issue", Some("list")) | ("issue", Some("view"))
                | ("repo", Some("view")) | ("project", Some("view"))
                | ("ci", Some("list")) | ("ci", Some("view")) | ("ci", Some("status"))
        )
    } else {
        matches!(
            (group, action),
            ("pr", Some("list")) | ("pr", Some("view")) | ("pr", Some("status")) | ("pr", Some("checks"))
                | ("issue", Some("list")) | ("issue", Some("view"))
                | ("repo", Some("view"))
                | ("run", Some("list")) | ("run", Some("view"))
                | ("ci", Some("list")) | ("ci", Some("view"))
        )
    };
    if is_read {
        return PolicyDecision::Allow;
    }

    // ---- 3. ASK LIST (write ops + api/graphql backdoors) ----
    // api / graphql are universal backdoors → always Ask.
    if group == "api" || group == "graphql" {
        return ask("api/graphql is an unrestricted backdoor — needs confirmation");
    }
    let write_group = if is_glab {
        matches!(group, "mr" | "issue" | "release" | "repo" | "project")
    } else {
        matches!(group, "pr" | "issue" | "release" | "repo")
    };
    if write_group {
        if let Some(act) = action {
            let is_write_action = matches!(
                act,
                "create" | "merge" | "close" | "delete" | "edit" | "ready"
                    | "review" | "upload" | "clone" | "fork" | "archive"
            );
            if is_write_action {
                return ask("write operation — needs confirmation");
            }
        }
    }

    // ---- 4. DEFAULT: fail-closed Ask ----
    ask("unknown or unlisted subcommand — needs confirmation")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    // ---- AC5: deny ----
    #[test]
    fn deny_gh_auth_token() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["auth", "token"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn deny_gh_auth_status_show_token() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["auth", "status", "--show-token"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn deny_gh_auth_setup_git() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["auth", "setup-git"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn deny_gh_auth_login_logout() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["auth", "login"])),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate_policy("gh", &argv(&["auth", "logout"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn deny_gh_secret_and_sshkey() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["secret", "set", "FOO"])),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate_policy("gh", &argv(&["ssh-key", "add"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn deny_glab_auth_token() {
        assert!(matches!(
            evaluate_policy("glab", &argv(&["auth", "token"])),
            PolicyDecision::Deny { .. }
        ));
    }

    // ---- AC5: allow ----
    #[test]
    fn allow_gh_pr_list() {
        assert_eq!(evaluate_policy("gh", &argv(&["pr", "list"])), PolicyDecision::Allow);
    }

    #[test]
    fn allow_gh_read_variants() {
        for a in [
            &["pr", "view"][..],
            &["pr", "status"][..],
            &["pr", "checks"][..],
            &["issue", "list"][..],
            &["issue", "view"][..],
            &["repo", "view"][..],
            &["run", "list"][..],
        ] {
            assert_eq!(evaluate_policy("gh", &argv(a)), PolicyDecision::Allow, "argv={:?}", a);
        }
    }

    #[test]
    fn allow_glab_mr_list() {
        assert_eq!(evaluate_policy("glab", &argv(&["mr", "list"])), PolicyDecision::Allow);
    }

    #[test]
    fn allow_git_read_transport() {
        for s in ["fetch", "pull", "clone", "status", "log", "diff"] {
            assert_eq!(evaluate_policy("git", &argv(&[s])), PolicyDecision::Allow, "git {s}");
        }
    }

    #[test]
    fn allow_git_credential() {
        // credential helper protocol must be Allow (not Ask) so it never pops a
        // modal mid-fetch; host allowlist is the real gate.
        for a in [&["credential", "get"][..], &["credential", "store"][..], &["credential", "erase"][..]] {
            assert_eq!(evaluate_policy("git", &argv(a)), PolicyDecision::Allow, "argv={a:?}");
        }
    }

    // ---- AC5: ask ----
    #[test]
    fn ask_gh_pr_create() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["pr", "create", "--title", "x"])),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn ask_gh_api() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["api", "/user"])),
            PolicyDecision::Ask { .. }
        ));
        assert!(matches!(
            evaluate_policy("gh", &argv(&["graphql", "-f", "query=x"])),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn ask_git_push() {
        assert!(matches!(
            evaluate_policy("git", &argv(&["push", "origin", "main"])),
            PolicyDecision::Ask { .. }
        ));
    }

    // ---- AC5: unknown -> Ask (never silent-Allow) ----
    #[test]
    fn ask_unknown_subcommand() {
        assert!(matches!(
            evaluate_policy("gh", &argv(&["gist", "create"])),
            PolicyDecision::Ask { .. }
        ));
        assert!(matches!(
            evaluate_policy("git", &argv(&["reflog"])),
            PolicyDecision::Ask { .. }
        ));
        // bare auth status (no --show-token) is sensitive → Ask, not Allow
        assert!(matches!(
            evaluate_policy("gh", &argv(&["auth", "status"])),
            PolicyDecision::Ask { .. }
        ));
        // unknown tool → Ask
        assert!(matches!(
            evaluate_policy("hub", &argv(&["pr", "list"])),
            PolicyDecision::Ask { .. }
        ));
    }

    // ---- SEC-2 (loop-back 1): case-insensitive subcommand/action matching ----
    #[test]
    fn deny_gh_auth_token_case_insensitive() {
        // Upper-case must resolve to the same Deny as lower-case.
        for a in [
            &["AUTH", "TOKEN"][..],
            &["Auth", "Token"][..],
            &["auth", "TOKEN"][..],
            &["AUTH", "token"][..],
        ] {
            assert!(
                matches!(evaluate_policy("gh", &argv(a)), PolicyDecision::Deny { .. }),
                "argv={a:?} must Deny (was reaching Ask before SEC-2 fix)"
            );
        }
    }

    #[test]
    fn deny_auth_status_show_token_case_insensitive() {
        // Positionals uppercased, flag kept verbatim → still Deny.
        assert!(matches!(
            evaluate_policy("gh", &argv(&["AUTH", "STATUS", "--show-token"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn deny_glab_and_secret_groups_case_insensitive() {
        assert!(matches!(
            evaluate_policy("glab", &argv(&["AUTH", "Token"])),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate_policy("gh", &argv(&["Secret", "set", "FOO"])),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate_policy("gh", &argv(&["SSH-KEY", "add"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn allow_read_case_insensitive_still_allows() {
        // No regression for the allow path under mixed case.
        assert_eq!(
            evaluate_policy("gh", &argv(&["PR", "List"])),
            PolicyDecision::Allow
        );
    }
}
