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

use crate::db::Db;
use std::collections::HashSet;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

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
///
/// `IdentityAgent` is pinned **per Host** so agent-auth and key-auth servers use
/// the RIGHT ssh-agent even though the child process's ambient `SSH_AUTH_SOCK`
/// points at the per-run agent (SRS §3.3):
///   - `auth_method="key"`  → the per-run agent (`per_run_sock`, when present),
///     which holds the keys Devdy `ssh-add`ed (passphrase-protected included).
///   - `auth_method="agent"` → the USER's own agent (`user_sock`, when the user
///     has one), so a server that relies on the user's already-loaded keys still
///     authenticates instead of failing against an empty per-run agent. When the
///     user has NO ambient agent, no `IdentityAgent` is emitted for that Host —
///     ssh then fails cleanly for lack of a key (the honest outcome, not a
///     silent failure caused by shadowing the user's agent).
///
/// This is still a PURE function: it reads no environment; both socket paths are
/// passed in by the caller.
pub fn build_ssh_config(
    servers: &[MappedServer],
    user_sock: Option<&str>,
    per_run_sock: Option<&str>,
) -> String {
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
            // Key-auth servers use the per-run agent (holds ssh-add'ed keys).
            if let Some(sock) = per_run_sock.filter(|p| !p.is_empty()) {
                out.push_str(&format!("    IdentityAgent {sock}\n"));
            }
        } else if s.auth_method == "agent" {
            // Agent-auth servers inherit the user's OWN agent, not the empty
            // per-run one (SRS §3.3). No user agent → no IdentityAgent → ssh
            // fails cleanly rather than silently against a keyless agent.
            if let Some(sock) = user_sock.filter(|p| !p.is_empty()) {
                out.push_str(&format!("    IdentityAgent {sock}\n"));
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

/// A mapped server paired with its DB id. The id is kept OUT of `MappedServer`
/// (which flows into the pure builders) and used only to fetch the passphrase at
/// ssh-add time — never rendered anywhere (SEC-301).
struct MappedServerWithId {
    server_id: String,
    server: MappedServer,
}

/// Query the servers mapped to `project_id` (GĐ2 `project_servers` × `servers`)
/// and resolve them into `MappedServer`s with unique aliases. Mirrors
/// `list_project_servers`'s JOIN but scoped to the fields the ssh layer needs;
/// `WHERE ps.project_id = ?` enforces isolation (BR-301 / SEC-302). A server
/// mapped under multiple roles is de-duplicated by server id so it yields a
/// single Host block. `has_passphrase` is read from the Keychain-backed store
/// WITHOUT reading the value (SEC-301).
async fn mapped_servers_for_project(
    db: &Db,
    project_id: &str,
) -> Result<Vec<MappedServerWithId>, String> {
    use sqlx::Row;

    let rows = sqlx::query(
        "SELECT DISTINCT servers.id AS id, servers.label AS label, servers.host AS host, \
                servers.port AS port, servers.username AS username, \
                servers.auth_method AS auth_method, servers.private_key_path AS private_key_path \
         FROM project_servers ps \
         JOIN servers ON servers.id = ps.server_id \
         WHERE ps.project_id = ? \
         ORDER BY servers.label",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;

    let mut aliases: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let server_id: String = row.get("id");
        let label: String = row.get("label");
        let alias = slugify_alias(&label, &mut aliases);
        let has_passphrase = crate::secrets::has_server_secret(&server_id);
        out.push(MappedServerWithId {
            server_id,
            server: MappedServer {
                alias,
                host: row.get("host"),
                port: row.get("port"),
                username: row.get("username"),
                auth_method: row.get("auth_method"),
                private_key_path: row.get("private_key_path"),
                has_passphrase,
            },
        });
    }
    Ok(out)
}

/// Resolve the ssh context block for a project's mapped servers (AC-308). Empty
/// string when the project maps no server (AC-305 no-op). Called by
/// `build_account_context` (token.rs) so `start_run` is not touched.
pub async fn build_project_ssh_context(db: &Db, project_id: &str) -> String {
    match mapped_servers_for_project(db, project_id).await {
        Ok(list) => {
            let servers: Vec<MappedServer> = list.into_iter().map(|m| m.server).collect();
            build_ssh_context(&servers)
        }
        // Fail-soft: ssh context is not a secret and must never break a run.
        Err(_) => String::new(),
    }
}

/// RAII handle for a run's transparent-ssh resources. On Drop it kills the
/// per-run ssh-agent and removes the temp config dir, so no agent or key
/// material lingers after the run ends (§3.6). Stored in `RunHandles.ssh_access`.
pub struct SshAccessGuard {
    agent_pid: Option<u32>,
    dir: PathBuf,
}

impl Drop for SshAccessGuard {
    fn drop(&mut self) {
        // Kill the per-run agent (best-effort; the agent holds decrypted keys).
        if let Some(pid) = self.agent_pid {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        // Remove the temp dir (config + agent socket). Best-effort.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Prepare transparent SSH for one Claude run of `project_id`.
///
/// Returns `Ok(None)` when the project maps no server (AC-305 no-op): no config
/// is written, `DEVDY_SSH_CONFIG` is not set, no agent is spawned. Otherwise it:
///   - writes `<app_data>/ssh-runs/<run_id>/config` (dir 0700, file 0600) from
///     `build_ssh_config`,
///   - spawns a per-run `ssh-agent -a <dir>/agent.sock`,
///   - for each `key` server with a stored passphrase, runs `ssh-add <key>` with
///     the passphrase supplied via `DEVDY_ASKPASS_PASS` on the ssh-add child env
///     + `SSH_ASKPASS`/`SSH_ASKPASS_REQUIRE=force` (SEC-301: never argv/file/log),
///   - sets `DEVDY_SSH_CONFIG` + `SSH_AUTH_SOCK` on `cmd` (the shim dir is already
///     on PATH via `prepend_shim_path` in `wire_broker`),
///   - returns an `SshAccessGuard` to store in `RunHandles`.
///
/// Fail-soft (BR-303): agent/ssh-add failures are logged as warnings (NEVER the
/// passphrase) and do not abort the run — ssh may still fail at use time and the
/// agent sees the error in output.
pub async fn prepare_ssh_access(
    app: &AppHandle,
    db: &Db,
    cmd: &mut Command,
    run_id: &str,
    project_id: &str,
) -> Result<Option<SshAccessGuard>, String> {
    let mapped = mapped_servers_for_project(db, project_id).await?;
    if mapped.is_empty() {
        return Ok(None); // AC-305: no-op.
    }

    // Per-run scratch dir under app data: <app_data>/ssh-runs/<run_id>/.
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?;
    let dir = base.join("ssh-runs").join(run_id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    // Capture the user's OWN ssh-agent socket (if any) BEFORE we override
    // SSH_AUTH_SOCK on `cmd`, so agent-auth servers can keep inheriting it via a
    // per-Host IdentityAgent (SRS §3.3). Read from this process's environment,
    // which the run inherits by default; None when the user has no agent.
    let user_sock = std::env::var("SSH_AUTH_SOCK")
        .ok()
        .filter(|s| !s.is_empty());

    // Spawn a per-run ssh-agent bound to a socket inside the temp dir. Do this
    // BEFORE writing the config so the config can pin key-auth Hosts at the
    // per-run agent only when it actually came up (fail-soft: if the agent did
    // not start, key servers still fall back to their IdentityFile).
    let sock_path = dir.join("agent.sock");
    let agent_pid = spawn_agent(&sock_path).await;
    let per_run_sock = if agent_pid.is_some() {
        Some(sock_path.to_string_lossy().into_owned())
    } else {
        None
    };

    // Write the ssh config (0600). Contains no secret (SEC-301).
    let servers: Vec<MappedServer> = mapped.iter().map(|m| m.server.clone()).collect();
    let config_body = build_ssh_config(&servers, user_sock.as_deref(), per_run_sock.as_deref());
    let config_path = dir.join("config");
    std::fs::write(&config_path, config_body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600));
    }

    // Load passphrase-protected keys into the agent via askpass. Fail-soft.
    if let Some(_pid) = agent_pid {
        for m in &mapped {
            if m.server.auth_method != "key" || !m.server.has_passphrase {
                continue; // agent auth / no passphrase → nothing to add here.
            }
            let Some(key_path) = m.server.private_key_path.as_deref().filter(|p| !p.is_empty())
            else {
                continue;
            };
            add_key(app, db, &sock_path, &m.server_id, key_path).await;
        }
    }

    // Point the sidecar (and the ssh/scp shim) at this run's config + agent.
    cmd.env("DEVDY_SSH_CONFIG", &config_path);
    if agent_pid.is_some() {
        cmd.env("SSH_AUTH_SOCK", &sock_path);
    }

    Ok(Some(SshAccessGuard { agent_pid, dir }))
}

/// Spawn `ssh-agent -a <sock>` and return its pid. Returns `None` (fail-soft) if
/// the agent could not be started; the config still lets key-without-passphrase
/// and agent-inherited auth work.
async fn spawn_agent(sock_path: &std::path::Path) -> Option<u32> {
    // `ssh-agent -a <sock>` daemonizes and prints the agent pid; we parse it so
    // the guard can kill it. `-D` (foreground) would block, so use the default.
    let out = Command::new("ssh-agent")
        .arg("-a")
        .arg(sock_path)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            parse_agent_pid(&text)
        }
        Ok(_) | Err(_) => {
            eprintln!("devdy: ssh-agent failed to start for run (transparent ssh degraded)");
            None
        }
    }
}

/// Parse the `SSH_AGENT_PID=<n>;` line ssh-agent prints on stdout.
fn parse_agent_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        if let Some(rest) = line.split_once("SSH_AGENT_PID=") {
            let digits: String = rest.1.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(pid) = digits.parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

/// Run `ssh-add <key>` against the per-run agent, supplying the passphrase via
/// the child's `DEVDY_ASKPASS_PASS` env + the devdy-askpass helper (SEC-301: the
/// passphrase is never on argv, never in a file, never logged). Fail-soft.
async fn add_key(app: &AppHandle, _db: &Db, sock_path: &std::path::Path, server_id: &str, key_path: &str) {
    // Read the passphrase from the Keychain-backed store at the last moment.
    let secret = crate::secrets::get_server_secret(server_id);
    let Some(passphrase) = secret.passphrase else {
        return; // has_passphrase was true but value missing → skip (fail-soft).
    };

    let askpass = match resolve_askpass(app) {
        Some(p) => p,
        None => {
            eprintln!("devdy: askpass helper not found; key with passphrase not loaded");
            return;
        }
    };

    let mut cmd = Command::new("ssh-add");
    cmd.arg(key_path)
        .env("SSH_AUTH_SOCK", sock_path)
        .env("SSH_ASKPASS", &askpass)
        // OpenSSH >= 8.4: force askpass even with no tty/DISPLAY (headless).
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", ":0")
        // The passphrase travels ONLY on this child process's env (SEC-301).
        .env("DEVDY_ASKPASS_PASS", &passphrase)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match cmd.status().await {
        Ok(s) if s.success() => {}
        // NEVER log the passphrase; only that the add failed.
        _ => eprintln!("devdy: ssh-add failed for a mapped key (transparent ssh degraded)"),
    }
}

/// Resolve the devdy-askpass helper next to the sidecar shims. Mirrors
/// `resolve_shim_dir` resolution order: DEVDY_SHIM_DIR → bundled resource dir →
/// dev fallback.
fn resolve_askpass(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DEVDY_SHIM_DIR") {
        let cand = PathBuf::from(p).join("devdy-askpass");
        if cand.exists() {
            return Some(cand);
        }
    }
    if let Ok(res) = app.path().resource_dir() {
        let cand = res.join("sidecar-proxy").join("devdy-askpass");
        if cand.exists() {
            return Some(cand);
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("sidecar-proxy")
        .join("devdy-askpass");
    if dev.exists() {
        Some(dev)
    } else {
        None
    }
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
        let cfg = build_ssh_config(&servers, None, None);

        // Exactly two Host blocks.
        assert_eq!(cfg.matches("Host ").count(), 2);
        assert!(cfg.contains("Host s1\n"));
        assert!(cfg.contains("Host s2\n"));
        // S1 carries the IdentityFile; S2 (agent) does not.
        assert!(cfg.contains("    IdentityFile /home/u/.ssh/id_ed25519\n"));
        assert_eq!(cfg.matches("IdentityFile ").count(), 1);
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
        let cfg = build_ssh_config(&servers, None, None);
        assert!(cfg.contains("Host s1\n"));
        assert!(!cfg.contains("IdentityFile"));
    }

    #[test]
    fn build_ssh_config_empty_is_noop() {
        // AC-305: no servers → empty config (caller writes nothing).
        assert_eq!(build_ssh_config(&[], None, None), "");
    }

    #[test]
    fn build_ssh_config_never_contains_passphrase() {
        // SEC-301 / AC-306: MappedServer has no passphrase field, so a
        // passphrase-shaped token can never appear in the rendered config.
        let servers = vec![
            key_server("s1", "h1", Some("/k1"), true),
            agent_server("s2", "h2"),
        ];
        let cfg = build_ssh_config(&servers, Some("/run/user.sock"), Some("/run/per-run.sock"));
        assert!(!cfg.to_lowercase().contains("passphrase"));
        assert!(!cfg.contains("has_passphrase"));
    }

    // ---- SRS §3.3: per-Host IdentityAgent picks the right agent per auth_method ----

    #[test]
    fn build_ssh_config_agent_host_uses_user_sock_key_host_uses_per_run_sock() {
        // Mixed run: a key-auth server + an agent-auth server. The key Host must
        // pin the per-run agent (holds ssh-add'ed keys); the agent Host must pin
        // the user's OWN agent so it keeps working (SRS §3.3), NOT the empty
        // per-run agent that would shadow it.
        let servers = vec![
            key_server("kx", "203.0.113.10", Some("/home/u/.ssh/id_ed25519"), true),
            agent_server("ax", "198.51.100.5"),
        ];
        let cfg = build_ssh_config(&servers, Some("/run/user.sock"), Some("/run/per-run.sock"));

        // The key Host block pins the per-run agent.
        let kx = cfg.split("Host ax\n").next().unwrap();
        assert!(kx.contains("Host kx\n"));
        assert!(kx.contains("    IdentityAgent /run/per-run.sock\n"));
        assert!(!kx.contains("/run/user.sock"));

        // The agent Host block pins the USER's agent.
        let ax = &cfg[cfg.find("Host ax\n").unwrap()..];
        assert!(ax.contains("    IdentityAgent /run/user.sock\n"));
        assert!(!ax.contains("/run/per-run.sock"));

        // Two IdentityAgent lines total (one per Host), the right sock each.
        assert_eq!(cfg.matches("IdentityAgent").count(), 2);
    }

    #[test]
    fn build_ssh_config_agent_host_without_user_sock_omits_identity_agent() {
        // No ambient user agent → the agent-auth Host emits NO IdentityAgent, so
        // ssh fails cleanly for lack of a key rather than silently against the
        // empty per-run agent that would otherwise shadow the (absent) user one.
        let servers = vec![agent_server("ax", "198.51.100.5")];
        let cfg = build_ssh_config(&servers, None, Some("/run/per-run.sock"));
        assert!(cfg.contains("Host ax\n"));
        assert!(!cfg.contains("IdentityAgent"));
    }

    #[test]
    fn build_ssh_config_key_host_without_per_run_sock_omits_identity_agent() {
        // Agent failed to start (per_run_sock None) → key Host keeps only its
        // IdentityFile (fail-soft fallback), no IdentityAgent line.
        let servers = vec![key_server("kx", "h", Some("/k"), false)];
        let cfg = build_ssh_config(&servers, Some("/run/user.sock"), None);
        assert!(cfg.contains("    IdentityFile /k\n"));
        assert!(!cfg.contains("IdentityAgent"));
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

    // ---- ssh-agent pid parsing (orchestration helper) ----

    #[test]
    fn parse_agent_pid_reads_the_pid_line() {
        let out = "SSH_AUTH_SOCK=/tmp/run/agent.sock; export SSH_AUTH_SOCK;\n\
                   SSH_AGENT_PID=45231; export SSH_AGENT_PID;\n\
                   echo Agent pid 45231;\n";
        assert_eq!(parse_agent_pid(out), Some(45231));
    }

    #[test]
    fn parse_agent_pid_none_when_absent() {
        assert_eq!(parse_agent_pid("no pid here\n"), None);
    }

    // ---- mapped_servers_for_project: isolation + empty (BR-301 / AC-305) ----

    use sqlx::sqlite::SqlitePoolOptions;

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("mem db");
        sqlx::query(
            "CREATE TABLE servers (id TEXT PRIMARY KEY, label TEXT, host TEXT, port INTEGER, \
             username TEXT, auth_method TEXT, private_key_path TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE project_servers (project_id TEXT, server_id TEXT, role TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn mapped_servers_empty_when_project_maps_none() {
        // AC-305: no mapping → empty list → context is empty (no-op).
        let db = mem_db().await;
        let list = mapped_servers_for_project(&db, "p1").await.unwrap();
        assert!(list.is_empty());
        assert_eq!(build_project_ssh_context(&db, "p1").await, "");
    }

    #[tokio::test]
    async fn mapped_servers_isolated_to_project() {
        // BR-301 / SEC-302: only this project's servers are returned.
        let db = mem_db().await;
        sqlx::query(
            "INSERT INTO servers (id, label, host, port, username, auth_method, private_key_path) \
             VALUES ('s1','Web','h1',22,'deploy','key','/k'), \
                    ('s2','Other','h2',22,'root','agent',NULL)",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO project_servers (project_id, server_id, role) VALUES ('p1','s1','production')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO project_servers (project_id, server_id, role) VALUES ('p2','s2','production')")
            .execute(&db)
            .await
            .unwrap();

        let list = mapped_servers_for_project(&db, "p1").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].server.alias, "web");
        assert_eq!(list[0].server.host, "h1");
        // The other project's server never leaks into the context.
        let ctx = build_project_ssh_context(&db, "p1").await;
        assert!(ctx.contains("`web`"));
        assert!(!ctx.contains("h2"));
    }

    #[tokio::test]
    async fn mapped_servers_dedupes_multi_role_mapping() {
        // A server mapped under two roles yields one Host entry (one alias).
        let db = mem_db().await;
        sqlx::query(
            "INSERT INTO servers (id, label, host, port, username, auth_method, private_key_path) \
             VALUES ('s1','Web','h1',22,'deploy','key','/k')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO project_servers (project_id, server_id, role) VALUES ('p1','s1','production'),('p1','s1','staging')")
            .execute(&db)
            .await
            .unwrap();
        let list = mapped_servers_for_project(&db, "p1").await.unwrap();
        assert_eq!(list.len(), 1, "multi-role mapping must de-dupe to one server");
    }
}
