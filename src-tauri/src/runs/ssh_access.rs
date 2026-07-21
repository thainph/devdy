//! Transparent SSH access for a Claude run (ssh-transparent-connect pivot).
//!
//! When a project has VPS servers mapped (GĐ2), a Claude run is given a per-run
//! ssh config + a private ssh-agent so the agent can `ssh <alias> '<cmd>'` with
//! zero interaction — the same "credential mediated per project" model as the
//! gh/glab broker. Only servers mapped to THIS project appear (BR-301 isolation).
//!
//! This module is split into two layers:
//!   - PURE builders (`slugify_alias`, `build_ssh_config`, `build_ssh_context`)
//!     that take an already-resolved `&[MappedServer]` → `String`. They touch no
//!     DB / Keychain / filesystem and are exhaustively unit-tested. By
//!     construction they can NEVER see a passphrase: `MappedServer` carries only
//!     `has_passphrase` (a bool), never the secret value (SEC-301 / AC-306).
//!   - The async orchestration (`prepare_ssh_access` + `SshAccessGuard`, in T4)
//!     that queries the mapping, writes the config, spawns the agent and loads
//!     keys via askpass — verified at runtime (NFR-304), not in these unit tests.

use std::collections::HashSet;

/// One project-mapped server, ready to render into an ssh config `Host` block.
///
/// NO passphrase field by construction (SEC-301): the passphrase never reaches
/// the pure config/context builders — it only lives inside `prepare_ssh_access`
/// at ssh-add time. `has_passphrase` merely drives whether an ssh-add-via-askpass
/// step runs; it is not a secret.
#[derive(Debug, Clone)]
pub struct MappedServer {
    /// Slug derived from the server label, unique within one run.
    pub alias: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    /// `"agent"` | `"key"`.
    pub auth_method: String,
    /// Absolute path to the private key. Only meaningful for `auth_method="key"`.
    pub private_key_path: Option<String>,
    /// Whether a passphrase is stored for this server (drives ssh-add). NOT the
    /// passphrase value.
    pub has_passphrase: bool,
}

/// Slug-ify `label` into an ssh alias (`a-z0-9-`, collapsing runs of separators,
/// trimming leading/trailing `-`). A label that is empty or made entirely of
/// non-alphanumeric characters falls back to `"server"`. If the result already
/// exists in `existing`, a `-2`, `-3`… suffix is appended until it is unique.
/// The returned alias is inserted into `existing` so repeated calls accumulate
/// uniqueness (AC-304).
pub fn slugify_alias(label: &str, existing: &mut HashSet<String>) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in label.chars() {
        let lc = ch.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            slug.push(lc);
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            // Any other char becomes a single separating dash (collapsed).
            slug.push('-');
            prev_dash = true;
        }
    }
    // Trim a trailing dash left by a separator at the end of the label.
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug = "server".to_string();
    }

    // Ensure uniqueness against previously assigned aliases.
    let mut candidate = slug.clone();
    let mut n = 2;
    while existing.contains(&candidate) {
        candidate = format!("{slug}-{n}");
        n += 1;
    }
    existing.insert(candidate.clone());
    candidate
}

/// Render the full per-run ssh config from an already-aliased server list.
/// Empty input → `""` (no-op: caller writes no file, sets no `DEVDY_SSH_CONFIG`
/// — AC-305). One `Host` block per server; `IdentityFile` is emitted ONLY for
/// `auth_method="key"` with a non-empty path. Always sets `BatchMode yes` +
/// `StrictHostKeyChecking accept-new` + `IdentitiesOnly yes` so ssh never
/// prompts (BR-303). NEVER contains a passphrase (SEC-301): `MappedServer` has
/// no passphrase field.
pub fn build_ssh_config(servers: &[MappedServer]) -> String {
    let mut out = String::new();
    for s in servers {
        out.push_str(&format!("Host {}\n", s.alias));
        out.push_str(&format!("    HostName {}\n", s.host));
        out.push_str(&format!("    User {}\n", s.username));
        out.push_str(&format!("    Port {}\n", s.port));
        out.push_str("    IdentitiesOnly yes\n");
        out.push_str("    StrictHostKeyChecking accept-new\n");
        out.push_str("    BatchMode yes\n");
        if s.auth_method == "key" {
            if let Some(path) = s.private_key_path.as_deref().filter(|p| !p.is_empty()) {
                out.push_str(&format!("    IdentityFile {path}\n"));
            }
        }
        out.push('\n');
    }
    out
}

/// Build the appended system-prompt block listing the run's available servers so
/// Claude knows the aliases + how to use them (AC-308). Empty input → `""` so
/// the caller appends nothing (AC-305 no-op). Contains only public metadata
/// (alias/host/user) — NEVER a secret (SEC-301).
pub fn build_ssh_context(servers: &[MappedServer]) -> String {
    if servers.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(
        "This project has managed VPS servers pre-wired for transparent SSH by Devdy. \
         You can connect to them without entering any secret (auth is already \
         configured on this run):\n",
    );
    for s in servers {
        out.push_str(&format!(
            "- `{}` (host `{}`, user `{}`)\n",
            s.alias, s.host, s.username
        ));
    }
    out.push_str(
        "\nRun `ssh <alias> '<command>'` (e.g. `ssh ",
    );
    // Safe: servers is non-empty here.
    out.push_str(&servers[0].alias);
    out.push_str(
        " 'uname -a'`) or `scp` as usual — the connection, host key and key auth \
         are handled for you. Only these mapped servers are reachable.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_server(alias: &str, host: &str, path: Option<&str>, has_pass: bool) -> MappedServer {
        MappedServer {
            alias: alias.to_string(),
            host: host.to_string(),
            port: 22,
            username: "deploy".to_string(),
            auth_method: "key".to_string(),
            private_key_path: path.map(str::to_string),
            has_passphrase: has_pass,
        }
    }

    fn agent_server(alias: &str, host: &str) -> MappedServer {
        MappedServer {
            alias: alias.to_string(),
            host: host.to_string(),
            port: 2222,
            username: "root".to_string(),
            auth_method: "agent".to_string(),
            private_key_path: None,
            has_passphrase: false,
        }
    }

    // ---- AC-303: config renders the right fields, isolates unmapped servers ----

    #[test]
    fn build_ssh_config_two_blocks_with_and_without_identity_file() {
        // S1 key+path, S2 agent — mirrors the AC-303 scenario.
        let servers = vec![
            key_server("s1", "203.0.113.10", Some("/home/u/.ssh/id_ed25519"), true),
            agent_server("s2", "198.51.100.5"),
        ];
        let cfg = build_ssh_config(&servers);

        // Exactly two Host blocks.
        assert_eq!(cfg.matches("Host ").count(), 2);
        assert!(cfg.contains("Host s1\n"));
        assert!(cfg.contains("Host s2\n"));
        // S1 carries the IdentityFile; S2 (agent) does not.
        assert!(cfg.contains("    IdentityFile /home/u/.ssh/id_ed25519\n"));
        assert_eq!(cfg.matches("IdentityFile").count(), 1);
        // Both carry the non-interactive hardening.
        assert_eq!(cfg.matches("BatchMode yes").count(), 2);
        assert_eq!(cfg.matches("StrictHostKeyChecking accept-new").count(), 2);
        assert_eq!(cfg.matches("IdentitiesOnly yes").count(), 2);
        // Connection fields.
        assert!(cfg.contains("HostName 203.0.113.10"));
        assert!(cfg.contains("HostName 198.51.100.5"));
        assert!(cfg.contains("User deploy"));
        assert!(cfg.contains("User root"));
        assert!(cfg.contains("Port 22"));
        assert!(cfg.contains("Port 2222"));
        // A server not in the list never appears (BR-301 isolation).
        assert!(!cfg.contains("Host s3"));
    }

    #[test]
    fn build_ssh_config_key_without_path_omits_identity_file() {
        // auth_method="key" but no path → still a valid block, no IdentityFile.
        let servers = vec![key_server("s1", "h", None, false)];
        let cfg = build_ssh_config(&servers);
        assert!(cfg.contains("Host s1\n"));
        assert!(!cfg.contains("IdentityFile"));
    }

    #[test]
    fn build_ssh_config_empty_is_noop() {
        // AC-305: no servers → empty config (caller writes nothing).
        assert_eq!(build_ssh_config(&[]), "");
    }

    #[test]
    fn build_ssh_config_never_contains_passphrase() {
        // SEC-301 / AC-306: MappedServer has no passphrase field, so a
        // passphrase-shaped token can never appear in the rendered config.
        let servers = vec![
            key_server("s1", "h1", Some("/k1"), true),
            agent_server("s2", "h2"),
        ];
        let cfg = build_ssh_config(&servers);
        assert!(!cfg.to_lowercase().contains("passphrase"));
        assert!(!cfg.contains("has_passphrase"));
    }

    // ---- AC-304: alias slug + uniqueness ----

    #[test]
    fn slugify_alias_is_deterministic_and_lowercased() {
        let mut seen = HashSet::new();
        assert_eq!(slugify_alias("My Server", &mut seen), "my-server");
    }

    #[test]
    fn slugify_alias_dedupes_duplicate_labels() {
        // AC-304: two identical labels → unique aliases with -2 suffix.
        let mut seen = HashSet::new();
        assert_eq!(slugify_alias("My Server", &mut seen), "my-server");
        assert_eq!(slugify_alias("My Server", &mut seen), "my-server-2");
        assert_eq!(slugify_alias("My Server", &mut seen), "my-server-3");
    }

    #[test]
    fn slugify_alias_strips_exotic_chars_and_collapses_dashes() {
        let mut seen = HashSet::new();
        assert_eq!(slugify_alias("Prod  Web!!Server", &mut seen), "prod-web-server");
        assert_eq!(slugify_alias("  leading/trailing  ", &mut seen), "leading-trailing");
        // Only a-z0-9- survive.
        assert_eq!(slugify_alias("héllo_wörld", &mut seen), "h-llo-w-rld");
    }

    #[test]
    fn slugify_alias_empty_or_symbol_only_falls_back_to_server() {
        let mut seen = HashSet::new();
        assert_eq!(slugify_alias("", &mut seen), "server");
        assert_eq!(slugify_alias("@@@", &mut seen), "server-2");
    }

    // ---- AC-308 / AC-305: context block ----

    #[test]
    fn build_ssh_context_empty_is_noop() {
        // AC-305: no servers → empty context (caller appends nothing).
        assert_eq!(build_ssh_context(&[]), "");
    }

    #[test]
    fn build_ssh_context_lists_alias_host_user_and_usage() {
        // AC-308: alias/host/user + the `ssh <alias>` usage hint.
        let servers = vec![
            key_server("web", "203.0.113.10", Some("/k"), true),
            agent_server("db", "198.51.100.5"),
        ];
        let ctx = build_ssh_context(&servers);
        assert!(ctx.contains("`web`"));
        assert!(ctx.contains("`db`"));
        assert!(ctx.contains("203.0.113.10"));
        assert!(ctx.contains("198.51.100.5"));
        assert!(ctx.contains("deploy"));
        assert!(ctx.contains("root"));
        assert!(ctx.contains("ssh <alias>"));
        // Usage example uses the first server's alias.
        assert!(ctx.contains("ssh web"));
    }

    #[test]
    fn build_ssh_context_never_contains_passphrase() {
        // SEC-301 / AC-306.
        let servers = vec![key_server("web", "h", Some("/k"), true)];
        let ctx = build_ssh_context(&servers);
        assert!(!ctx.to_lowercase().contains("passphrase"));
    }
}
