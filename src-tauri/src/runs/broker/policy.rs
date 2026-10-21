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
    PolicyDecision::Deny {
        reason: reason.to_string(),
    }
}

fn ask(reason: &str) -> PolicyDecision {
    PolicyDecision::Ask {
        reason: reason.to_string(),
    }
}

/// Evaluate the broker policy for a tool + argv. Pure, synchronous, no I/O.
///
/// `tool` is one of `gh`, `glab`, `git`, `aws`, `terraform`, `az` (also accepts provider aliases
/// for defensiveness). `argv` is the argument vector AFTER the tool name.
pub fn evaluate_policy(tool: &str, argv: &[String]) -> PolicyDecision {
    match tool {
        "git" => evaluate_git(argv),
        "gh" | "github" => evaluate_gh_like(argv, false),
        "glab" | "gitlab" => evaluate_gh_like(argv, true),
        "aws" => evaluate_aws(argv),
        "terraform" => evaluate_terraform(argv),
        "az" => evaluate_az(argv),
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
    // `pr diff` / `mr diff` are pure reads (they only print the patch) and are
    // the main thing an in-session agent needs when the user pastes a PR link,
    // so they belong here rather than falling through to the default Ask.
    // `pr view --comments` needs no extra entry: flags don't affect the match.
    let is_read = if is_glab {
        matches!(
            (group, action),
            ("mr", Some("list"))
                | ("mr", Some("view"))
                | ("mr", Some("diff"))
                | ("issue", Some("list"))
                | ("issue", Some("view"))
                | ("repo", Some("view"))
                | ("project", Some("view"))
                | ("ci", Some("list"))
                | ("ci", Some("view"))
                | ("ci", Some("status"))
        )
    } else {
        matches!(
            (group, action),
            ("pr", Some("list"))
                | ("pr", Some("view"))
                | ("pr", Some("diff"))
                | ("pr", Some("status"))
                | ("pr", Some("checks"))
                | ("issue", Some("list"))
                | ("issue", Some("view"))
                | ("repo", Some("view"))
                | ("run", Some("list"))
                | ("run", Some("view"))
                | ("ci", Some("list"))
                | ("ci", Some("view"))
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
                "create"
                    | "merge"
                    | "close"
                    | "delete"
                    | "edit"
                    | "ready"
                    | "review"
                    | "upload"
                    | "clone"
                    | "fork"
                    | "archive"
            );
            if is_write_action {
                return ask("write operation — needs confirmation");
            }
        }
    }

    // ---- 4. DEFAULT: fail-closed Ask ----
    ask("unknown or unlisted subcommand — needs confirmation")
}

/// AWS CLI policy. Shape is usually `aws <service> <operation> ...`, except
/// global commands such as `configure`. The policy is intentionally
/// conservative: credential/auth mutation denies, obvious reads allow, writes
/// ask, unknowns ask.
fn evaluate_aws(argv: &[String]) -> PolicyDecision {
    let (pos, _flags) = split_argv(argv);
    let service = match pos.first() {
        Some(s) => s.as_str(),
        None => return ask("empty aws command — needs confirmation"),
    };
    let op = pos.get(1).map(|s| s.as_str()).unwrap_or("");

    if service == "configure" {
        return deny("`aws configure` mutates local credential/config state");
    }
    if service == "iam"
        && matches!(
            op,
            "create-access-key"
                | "delete-access-key"
                | "update-access-key"
                | "create-login-profile"
                | "update-login-profile"
                | "delete-login-profile"
                | "create-user"
                | "delete-user"
        )
    {
        return deny("AWS IAM credential/user management is blocked");
    }
    if service == "sts" && matches!(op, "get-session-token" | "assume-role") {
        return deny("AWS STS credential minting is blocked");
    }

    if service == "sts" && op == "get-caller-identity" {
        return PolicyDecision::Allow;
    }
    if service == "credential" && op == "get" {
        return PolicyDecision::Allow;
    }
    if op.starts_with("describe-")
        || op.starts_with("list-")
        || op.starts_with("get-")
        || op.starts_with("lookup-")
    {
        return PolicyDecision::Allow;
    }

    if service == "s3" {
        return match op {
            "ls" => PolicyDecision::Allow,
            "cp" | "sync" | "rm" | "mv" | "mb" | "rb" => {
                ask("aws s3 write operation — needs confirmation")
            }
            _ => ask("unknown or unlisted aws s3 command — needs confirmation"),
        };
    }

    let write_prefixes = [
        "create-",
        "update-",
        "delete-",
        "put-",
        "run-",
        "start-",
        "stop-",
        "terminate-",
        "reboot-",
        "attach-",
        "detach-",
        "authorize-",
        "revoke-",
        "modify-",
        "enable-",
        "disable-",
        "register-",
        "deregister-",
        "deploy",
        "publish",
    ];
    if write_prefixes.iter().any(|prefix| op.starts_with(prefix)) {
        return ask("AWS write operation — needs confirmation");
    }

    ask("unknown or unlisted aws command — needs confirmation")
}

/// Terraform policy. Shape is `terraform [global flags] <command> [sub] ...`;
/// global flags such as `-chdir=...` are dropped by `split_argv`. Anything that
/// can change real infrastructure or remote state asks; read/plan commands
/// allow so an agent can iterate on `plan` without a modal per call.
fn evaluate_terraform(argv: &[String]) -> PolicyDecision {
    let (pos, flags) = split_argv(argv);
    let command = match pos.first() {
        Some(c) => c.as_str(),
        // Bare `terraform` / `terraform -help` / `terraform -version`.
        None => return PolicyDecision::Allow,
    };
    let sub = pos.get(1).map(|s| s.as_str()).unwrap_or("");

    match command {
        // `init -migrate-state` / `-force-copy` rewrite the remote state backend.
        "init" => {
            if flags
                .iter()
                .any(|f| f.starts_with("-migrate-state") || f.starts_with("-force-copy"))
            {
                ask("terraform init state migration rewrites remote state — needs confirmation")
            } else {
                PolicyDecision::Allow
            }
        }
        // `output -json/-raw` and `show -json` print sensitive values in clear
        // (the human format redacts them as `<sensitive>`).
        "output" if flags.iter().any(|f| tf_flag_is(f, "json") || tf_flag_is(f, "raw")) => {
            ask("terraform output -json/-raw prints sensitive values — needs confirmation")
        }
        "show" if flags.iter().any(|f| tf_flag_is(f, "json")) => {
            ask("terraform show -json prints sensitive values — needs confirmation")
        }
        // `test` run blocks default to `command = apply` (real resources).
        "test" => ask("terraform test may apply real infrastructure — needs confirmation"),
        "plan" | "validate" | "fmt" | "show" | "output" | "providers" | "version" | "graph"
        | "get" | "modules" | "metadata" => PolicyDecision::Allow,
        "state" => match sub {
            "list" | "show" => PolicyDecision::Allow,
            // `state pull` dumps the raw state, secrets included.
            "pull" => ask("terraform state pull prints the raw state (secrets) — needs confirmation"),
            _ => ask("terraform state mutation — needs confirmation"),
        },
        "workspace" => match sub {
            "list" | "show" | "select" => PolicyDecision::Allow,
            _ => ask("terraform workspace change — needs confirmation"),
        },
        "apply" | "destroy" | "import" | "refresh" | "taint" | "untaint" | "force-unlock" => {
            ask("terraform operation changes infrastructure or state — needs confirmation")
        }
        _ => ask("unknown or unlisted terraform command — needs confirmation"),
    }
}

/// Escalate an `Allow` for a config-evaluating terraform command (`plan`,
/// `validate`, `console`) to `Ask` when the shim found code in the config that
/// runs outside Terraform (`data "external"`, `local-exec`, `data "http"`): it
/// would execute with the linked cloud credentials. Pure; `hits` are short
/// `file:kind` labels from the shim, used only in the reason.
pub fn escalate_terraform_external_code(
    decision: PolicyDecision,
    argv: &[String],
    external_code: bool,
    hits: &[String],
) -> PolicyDecision {
    if !external_code || decision != PolicyDecision::Allow {
        return decision;
    }
    let (pos, _flags) = split_argv(argv);
    if !matches!(
        pos.first().map(String::as_str),
        Some("plan" | "validate" | "console")
    ) {
        return decision;
    }
    let detail: Vec<String> = hits
        .iter()
        .take(5)
        .map(|h| {
            // `file:kind` → `kind in file`; keep labels short and printable.
            let h: String = h.chars().filter(|c| !c.is_control()).take(120).collect();
            match h.rsplit_once(':') {
                Some((file, kind)) if !file.is_empty() => format!("{kind} in {file}"),
                _ => h,
            }
        })
        .collect();
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({})", detail.join(", "))
    };
    ask(&format!(
        "terraform config runs external code{detail} with the linked cloud credentials — needs confirmation"
    ))
}

/// `-name`, `--name`, or `-name=...` (terraform accepts single or double dash).
fn tf_flag_is(flag: &str, name: &str) -> bool {
    let f = flag.trim_start_matches('-');
    f == name || f.starts_with(&format!("{name}="))
}

/// az global flags that take a value (`--output json`).
const AZ_GLOBAL_VALUE_FLAGS: &[&str] = &["--output", "-o", "--query", "--subscription"];
/// az global flags without a value.
const AZ_GLOBAL_BOOL_FLAGS: &[&str] = &["--debug", "--verbose", "--only-show-errors", "--help", "-h"];

/// Extract the az command path: positionals from the start of argv, skipping
/// global flags (and the VALUE of value-taking ones, incl. `--flag=value`), up
/// to the first command-specific flag. Flag values after the command therefore
/// never count as commands. Returns `(path, interleaved)` where `interleaved`
/// means a positional followed a global flag (e.g. `az -o json account show`).
fn az_command_path(argv: &[String]) -> (Vec<String>, bool) {
    let mut path = Vec::new();
    let mut seen_global = false;
    let mut interleaved = false;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a.starts_with('-') {
            let name = a.split('=').next().unwrap_or(a);
            if AZ_GLOBAL_VALUE_FLAGS.contains(&name) {
                seen_global = true;
                // `--flag value` consumes the next token; `--flag=value` does not.
                i += if a.contains('=') { 1 } else { 2 };
                continue;
            }
            if AZ_GLOBAL_BOOL_FLAGS.contains(&name) {
                seen_global = true;
                i += 1;
                continue;
            }
            // First command-specific flag ends the command path.
            break;
        }
        if seen_global {
            interleaved = true;
        }
        path.push(a.to_ascii_lowercase());
        i += 1;
    }
    (path, interleaved)
}

/// The `--subscription` value of an az invocation (`--subscription X` or
/// `--subscription=X`), if any. Used by the broker to compare against the
/// linked account's subscription.
pub fn az_subscription_arg(argv: &[String]) -> Option<String> {
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == "--subscription" {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix("--subscription=") {
            return Some(v.to_string());
        }
    }
    None
}

/// Azure CLI policy. Shape is `az <group> [subgroup...] <command> --flag value`.
/// The command path is ONLY the leading positionals before the first
/// `-`-prefixed token, so a flag value (`--resource-group show`) can never be
/// mistaken for a command. Credential/session mutation denies, secret-revealing
/// reads ask, plain reads allow, everything else asks (never silent-allow).
fn evaluate_az(argv: &[String]) -> PolicyDecision {
    let (path, interleaved) = az_command_path(argv);
    let p: Vec<&str> = path.iter().map(String::as_str).collect();

    // ---- 1. DENYLIST: session/credential mutation or credential printing ----
    // Checked first so no flag placement can soften a Deny into an Ask/Allow.
    match p.as_slice() {
        ["login", ..] | ["logout", ..] => {
            return deny("`az login`/`az logout` — the Azure session is managed by Devdy")
        }
        ["account", "get-access-token", ..] => {
            return deny("`az account get-access-token` prints a credential")
        }
        ["account", "set", ..] | ["account", "clear", ..] => {
            return deny("the Azure default subscription is managed by Devdy for this project")
        }
        ["ad", "sp", "credential", "reset", ..] | ["ad", "app", "credential", "reset", ..] => {
            return deny("Azure AD credential reset mints a secret and is blocked")
        }
        ["ad", "sp", "create-for-rbac", ..] => {
            return deny(
                "`az ad sp create-for-rbac` mints a service principal secret and is blocked",
            )
        }
        ["keyvault", "secret", "show", ..] | ["keyvault", "secret", "download", ..] => {
            return deny("reading Key Vault secret values is blocked")
        }
        _ => {}
    }

    // Help never executes the command.
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        return PolicyDecision::Allow;
    }
    if p.is_empty() {
        // Bare `az` / `az --version`; any other leading flag is not a valid az
        // invocation — do not guess.
        return if argv.is_empty() || argv.iter().all(|a| a == "--version") {
            PolicyDecision::Allow
        } else {
            ask("unrecognized az invocation — needs confirmation")
        };
    }
    // Global flags in front of / between command words: az itself rejects most
    // of these shapes, and the trailing word could be a disguised verb
    // (`az vm delete -o json list`), so never auto-allow them.
    if interleaved {
        return ask("az global flags before the command — needs confirmation");
    }

    // ---- 2. ASK: unrestricted backdoor / secret-revealing reads ----
    if p[0] == "rest" {
        return ask("`az rest` is an unrestricted backdoor — needs confirmation");
    }
    let command = *p.last().unwrap_or(&"");
    // Key Vault key/certificate metadata and secret NAMES carry no secret value.
    let keyvault_meta = p[0] == "keyvault"
        && matches!(p.get(1).copied(), Some("key" | "certificate" | "secret"))
        && p.len() == 3
        && matches!(command, "list" | "show" | "list-versions" | "list-deleted");
    if !keyvault_meta {
        const SECRET_WORDS: &[&str] = &[
            "key", "secret", "credential", "password", "token", "connection", "sas", "policy",
        ];
        let secret_path = p.iter().any(|seg| {
            // The `keyvault` group name itself is not a secret read.
            *seg != "keyvault" && SECRET_WORDS.iter().any(|w| seg.contains(w))
        });
        let secret_command = matches!(command, "list-publishing-profiles" | "get-credentials")
            || p.contains(&"appsettings")
            || p.contains(&"kubeconfig");
        // Application Insights `component show` includes the connection string.
        let app_insights = p.contains(&"app-insights") && command == "show";
        if secret_path || secret_command || app_insights {
            return ask("command may reveal a secret (keys/credentials) — needs confirmation");
        }
    }

    // ---- 3. READ ALLOWLIST ----
    if matches!(p.as_slice(), ["version"] | ["account", "show"] | ["account", "list"]) {
        return PolicyDecision::Allow;
    }
    if p.len() >= 2
        && (matches!(command, "list" | "show" | "exists" | "version" | "query")
            || command.starts_with("list-")
            || command.starts_with("show-"))
    {
        return PolicyDecision::Allow;
    }

    // ---- 4. DEFAULT: writes (create/update/delete/set/start/deploy/...) and
    // anything unknown ask.
    ask("Azure write or unlisted az command — needs confirmation")
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
        assert_eq!(
            evaluate_policy("gh", &argv(&["pr", "list"])),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn allow_gh_read_variants() {
        for a in [
            &["pr", "view"][..],
            &["pr", "diff"][..],
            &["pr", "status"][..],
            &["pr", "checks"][..],
            &["issue", "list"][..],
            &["issue", "view"][..],
            &["repo", "view"][..],
            &["run", "list"][..],
        ] {
            assert_eq!(
                evaluate_policy("gh", &argv(a)),
                PolicyDecision::Allow,
                "argv={:?}",
                a
            );
        }
    }

    #[test]
    fn allow_glab_mr_list() {
        assert_eq!(
            evaluate_policy("glab", &argv(&["mr", "list"])),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn allow_diff_and_view_with_flags() {
        // Reading a PR/MR from a pasted link must not pop a modal.
        assert_eq!(
            evaluate_policy("gh", &argv(&["pr", "diff", "42"])),
            PolicyDecision::Allow
        );
        assert_eq!(
            evaluate_policy("glab", &argv(&["mr", "diff", "42"])),
            PolicyDecision::Allow
        );
        assert_eq!(
            evaluate_policy("gh", &argv(&["pr", "view", "42", "--comments"])),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn allow_git_read_transport() {
        for s in ["fetch", "pull", "clone", "status", "log", "diff"] {
            assert_eq!(
                evaluate_policy("git", &argv(&[s])),
                PolicyDecision::Allow,
                "git {s}"
            );
        }
    }

    #[test]
    fn allow_git_credential() {
        // credential helper protocol must be Allow (not Ask) so it never pops a
        // modal mid-fetch; host allowlist is the real gate.
        for a in [
            &["credential", "get"][..],
            &["credential", "store"][..],
            &["credential", "erase"][..],
        ] {
            assert_eq!(
                evaluate_policy("git", &argv(a)),
                PolicyDecision::Allow,
                "argv={a:?}"
            );
        }
    }

    #[test]
    fn allow_aws_read_commands() {
        assert_eq!(
            evaluate_policy("aws", &argv(&["sts", "get-caller-identity"])),
            PolicyDecision::Allow
        );
        assert_eq!(
            evaluate_policy("aws", &argv(&["ec2", "describe-instances"])),
            PolicyDecision::Allow
        );
        assert_eq!(
            evaluate_policy("aws", &argv(&["credential", "get"])),
            PolicyDecision::Allow
        );
        assert_eq!(
            evaluate_policy("aws", &argv(&["s3", "ls"])),
            PolicyDecision::Allow
        );
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
    fn ask_aws_write_commands() {
        assert!(matches!(
            evaluate_policy("aws", &argv(&["ec2", "terminate-instances"])),
            PolicyDecision::Ask { .. }
        ));
        assert!(matches!(
            evaluate_policy("aws", &argv(&["s3", "sync", ".", "s3://bucket"])),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn deny_aws_credential_commands() {
        assert!(matches!(
            evaluate_policy("aws", &argv(&["configure"])),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate_policy("aws", &argv(&["iam", "create-access-key"])),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate_policy("aws", &argv(&["sts", "get-session-token"])),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn allow_terraform_read_and_plan() {
        for a in [
            &[][..],
            &["-version"][..],
            &["init"][..],
            &["init", "-reconfigure"][..],
            &["-chdir=envs/dev", "plan", "-out=tfplan"][..],
            &["plan", "-destroy"][..],
            &["validate"][..],
            &["fmt", "-check"][..],
            &["show", "tfplan"][..],
            &["output"][..],
            &["output", "vpc_id"][..],
            &["show"][..],
            &["state", "list"][..],
            &["state", "show", "aws_s3_bucket.this"][..],
            &["workspace", "select", "dev"][..],
        ] {
            assert_eq!(
                evaluate_policy("terraform", &argv(a)),
                PolicyDecision::Allow,
                "argv={a:?}"
            );
        }
    }

    #[test]
    fn ask_terraform_mutations() {
        for a in [
            &["apply", "tfplan"][..],
            &["apply", "-auto-approve"][..],
            &["-chdir=envs/dev", "destroy"][..],
            &["import", "aws_s3_bucket.this", "bucket"][..],
            &["refresh"][..],
            &["taint", "aws_instance.web"][..],
            &["force-unlock", "abc"][..],
            &["state", "rm", "aws_s3_bucket.this"][..],
            &["state", "mv", "a", "b"][..],
            &["state", "push", "x.tfstate"][..],
            &["workspace", "delete", "dev"][..],
            &["init", "-migrate-state"][..],
            &["init", "-force-copy"][..],
            &["login"][..],
            &["test"][..],
            &["test", "-filter=tests/a.tftest.hcl"][..],
            &["state", "pull"][..],
            &["output", "-json"][..],
            &["output", "-raw", "db_password"][..],
            &["output", "--json"][..],
            &["show", "-json"][..],
            &["show", "-json", "tfplan"][..],
        ] {
            assert!(
                matches!(
                    evaluate_policy("terraform", &argv(a)),
                    PolicyDecision::Ask { .. }
                ),
                "argv={a:?} must Ask"
            );
        }
    }

    // ---- az ----

    #[test]
    fn allow_az_reads() {
        for a in [
            &[][..],
            &["--version"][..],
            &["version"][..],
            &["account", "show"][..],
            &["account", "list", "-o", "table"][..],
            &["group", "list"][..],
            &["vm", "list", "--resource-group", "show"][..],
            &["vm", "list", "--resource-group", "create"][..],
            &["vm", "show", "-g", "rg", "-n", "delete"][..],
            &["storage", "account", "show", "-n", "x"][..],
            &["group", "exists", "-n", "rg"][..],
            &["vm", "list-sizes", "-l", "japaneast"][..],
            &["webapp", "show-something"][..],
            &["keyvault", "secret", "list", "--vault-name", "kv"][..],
            &["keyvault", "key", "show", "--vault-name", "kv", "-n", "k"][..],
            &["keyvault", "list"][..],
            &["keyvault", "show", "-n", "kv"][..],
            &["vm", "list", "-o", "json", "--query", "[].name"][..],
            &["vm", "list", "--subscription", "sub-1"][..],
            &["graph", "query", "-q", "Resources"][..],
            &["vm", "create", "--help"][..],
            &["ACCOUNT", "SHOW"][..],
        ] {
            assert_eq!(
                evaluate_policy("az", &argv(a)),
                PolicyDecision::Allow,
                "argv={a:?}"
            );
        }
    }

    #[test]
    fn deny_az_credential_and_session() {
        for a in [
            &["login"][..],
            &["login", "--tenant", "contoso.onmicrosoft.com"][..],
            &["logout"][..],
            &["account", "get-access-token"][..],
            &["account", "get-access-token", "--resource", "https://management.azure.com"][..],
            &["Account", "Get-Access-Token"][..],
            &["--only-show-errors", "account", "get-access-token"][..],
            &["-o", "json", "account", "get-access-token"][..],
            &["--output=json", "account", "get-access-token"][..],
            &["--query", "accessToken", "account", "get-access-token"][..],
            &["account", "--debug", "get-access-token"][..],
            &["--subscription", "x", "login"][..],
            &["account", "get-access-token", "--help"][..],
            &["account", "set", "--subscription", "other"][..],
            &["account", "clear"][..],
            &["ad", "sp", "credential", "reset", "--id", "x"][..],
            &["ad", "app", "credential", "reset", "--id", "x"][..],
            &["ad", "sp", "create-for-rbac", "-n", "x"][..],
            &["keyvault", "secret", "show", "--vault-name", "kv", "-n", "s"][..],
            &["keyvault", "secret", "download", "--vault-name", "kv", "-n", "s", "-f", "o"][..],
        ] {
            assert!(
                matches!(evaluate_policy("az", &argv(a)), PolicyDecision::Deny { .. }),
                "argv={a:?} must Deny"
            );
        }
    }

    #[test]
    fn ask_az_secret_reads_writes_and_unknown() {
        for a in [
            &["storage", "account", "keys", "list", "-n", "x"][..],
            &["cosmosdb", "keys", "list", "-n", "x", "-g", "rg"][..],
            &["redis", "list-keys", "-n", "x", "-g", "rg"][..],
            &["storage", "account", "show-connection-string", "-n", "x"][..],
            &["acr", "credential", "show", "-n", "x"][..],
            &["webapp", "deployment", "list-publishing-profiles", "-n", "x"][..],
            &["webapp", "config", "appsettings", "list", "-n", "x", "-g", "rg"][..],
            &["aks", "get-credentials", "-n", "x", "-g", "rg"][..],
            &["storage", "container", "generate-sas", "-n", "c"][..],
            &["rest", "--method", "get", "--url", "https://x"][..],
            &["group", "create", "-n", "rg", "-l", "japaneast"][..],
            &["vm", "delete", "-g", "rg", "-n", "list"][..],
            &["vm", "start", "-g", "rg", "-n", "x"][..],
            &["role", "assignment", "create"][..],
            &["deployment", "group", "create", "-g", "rg"][..],
            &["keyvault", "purge", "-n", "kv"][..],
            &["config", "set", "core.output=table"][..],
            &["extension", "add", "-n", "x"][..],
            &["find", "vm"][..],
            &["--only-show-errors", "account", "show"][..],
            &["vm", "delete", "-o", "json", "list"][..],
            &["list"][..],
            &["staticwebapp", "secrets", "list", "-n", "x"][..],
            &["communication", "list-key", "-n", "x"][..],
            &["iot", "hub", "policy", "list", "-n", "x"][..],
            &["keyvault", "secret", "show-deleted", "--vault-name", "kv", "-n", "s"][..],
            &["monitor", "app-insights", "component", "show", "-a", "x"][..],
            &["sql", "server", "show-connection-string", "-n", "x"][..],
            &["aks", "get-credentials", "-n", "x"][..],
            &["webapp", "config", "connection-string", "list", "-n", "x"][..],
            &["storage", "account", "generate-sas"][..],
            &["identity", "federated-credential", "list"][..],
        ] {
            assert!(
                matches!(evaluate_policy("az", &argv(a)), PolicyDecision::Ask { .. }),
                "argv={a:?} must Ask"
            );
        }
    }

    #[test]
    fn az_command_path_skips_global_flags_and_values() {
        let (p, inter) = az_command_path(&argv(&["vm", "list", "--resource-group", "show"]));
        assert_eq!(p, vec!["vm", "list"]);
        assert!(!inter);
        let (p, inter) = az_command_path(&argv(&["-o", "json", "account", "show"]));
        assert_eq!(p, vec!["account", "show"]);
        assert!(inter);
        let (p, _) = az_command_path(&argv(&["--query=[0]", "group", "list", "-g", "x"]));
        assert_eq!(p, vec!["group", "list"]);
    }

    #[test]
    fn az_subscription_arg_forms() {
        assert_eq!(
            az_subscription_arg(&argv(&["vm", "list", "--subscription", "Prod"])).as_deref(),
            Some("Prod")
        );
        assert_eq!(
            az_subscription_arg(&argv(&["vm", "list", "--subscription=abc"])).as_deref(),
            Some("abc")
        );
        assert!(az_subscription_arg(&argv(&["vm", "list"])).is_none());
    }

    #[test]
    fn terraform_external_code_escalates_evaluating_commands() {
        let hits = vec!["main.tf:data.external".to_string(), "modules/x/a.tf:local-exec".to_string()];
        for a in [&["plan"][..], &["-chdir=env", "validate"][..], &["plan", "-out=p"][..]] {
            let d = escalate_terraform_external_code(
                evaluate_policy("terraform", &argv(a)),
                &argv(a),
                true,
                &hits,
            );
            match d {
                PolicyDecision::Ask { reason } => {
                    assert!(reason.contains("data.external in main.tf"), "{reason}");
                    assert!(reason.contains("local-exec in modules/x/a.tf"), "{reason}");
                }
                other => panic!("argv={a:?} must Ask, got {other:?}"),
            }
        }
    }

    #[test]
    fn terraform_external_code_leaves_other_cases() {
        // No external code → unchanged Allow.
        let a = argv(&["plan"]);
        assert_eq!(
            escalate_terraform_external_code(PolicyDecision::Allow, &a, false, &[]),
            PolicyDecision::Allow
        );
        // Non-evaluating commands stay Allow even with external code.
        for c in [&["init"][..], &["fmt"][..], &["state", "list"][..], &["version"][..]] {
            assert_eq!(
                escalate_terraform_external_code(PolicyDecision::Allow, &argv(c), true, &[]),
                PolicyDecision::Allow,
                "argv={c:?}"
            );
        }
        // Deny/Ask are never softened or rewritten.
        let deny = PolicyDecision::Deny { reason: "x".into() };
        assert_eq!(
            escalate_terraform_external_code(deny.clone(), &a, true, &[]),
            deny
        );
        let ask_d = PolicyDecision::Ask { reason: "y".into() };
        assert_eq!(
            escalate_terraform_external_code(ask_d.clone(), &argv(&["apply"]), true, &[]),
            ask_d
        );
        // Flag without hits still asks, with a generic reason.
        assert!(matches!(
            escalate_terraform_external_code(PolicyDecision::Allow, &a, true, &[]),
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
