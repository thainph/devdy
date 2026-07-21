//! Credential broker (GĐ2).
//!
//! A per-run Unix-socket server that receives `{project_id,tool,argv,host}`
//! NDJSON requests from the (future GĐ3) `gh`/`glab`/git shim, applies the pure
//! policy engine (`policy`), consults an injectable `Approver` on `Ask`, resolves
//! the project's token fail-closed (`token`), and replies with
//! `{decision,reason?,token?}`. Every request is audited without the token.
//!
//! GĐ2 scope: this module stands alone and is unit-tested. It is NOT wired into
//! `runs.rs` — the run spawn path is unchanged (Hard Rule 8). GĐ3 will call
//! `start_broker` and pass a modal-backed `Approver`.

pub mod approver;
pub mod audit;
pub mod policy;
pub mod token;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::db::Db;
use approver::Approver;
use async_trait::async_trait;
use policy::PolicyDecision;

/// Request sent by the shim over the socket (one NDJSON line).
#[derive(Debug, Deserialize)]
pub struct BrokerRequest {
    pub project_id: String,
    pub tool: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub host: Option<String>,
    /// GĐ7: the run that issued this request. The app-wide singleton broker uses
    /// it to route an `Ask` to the right run's permission modal. Optional for
    /// backward-compat: absent → no live run to approve against, so any `Ask`
    /// fail-closed denies (reads/allows are unaffected).
    #[serde(default)]
    pub run_id: Option<String>,
}

/// Response returned to the shim (one NDJSON line). Secret fields are only
/// present when the decision is `allow` and credentials were resolved.
#[derive(Serialize)]
pub struct BrokerResponse {
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// GitHub/GitLab token for gh/glab/git. Secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Credential username for the git credential helper (NOT a secret). Only set
    /// on the `git credential get` branch; `None` (and thus omitted from JSON) for
    /// gh/glab so the existing shim contract is unchanged (backward-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// AWS credential fields for the aws shim / credential_process helper.
    /// `aws_secret_access_key` and `aws_session_token` are secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_access_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_secret_access_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_session_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,
}

impl BrokerResponse {
    fn deny(reason: impl Into<String>) -> Self {
        BrokerResponse {
            decision: "deny".into(),
            reason: Some(reason.into()),
            token: None,
            username: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            aws_profile: None,
            aws_region: None,
        }
    }
    fn allow(token: Option<String>) -> Self {
        BrokerResponse {
            decision: "allow".into(),
            reason: None,
            token,
            username: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            aws_profile: None,
            aws_region: None,
        }
    }
    fn allow_credential(token: String, username: String) -> Self {
        BrokerResponse {
            decision: "allow".into(),
            reason: None,
            token: Some(token),
            username: Some(username),
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            aws_profile: None,
            aws_region: None,
        }
    }
    fn allow_aws(creds: token::ResolvedAwsCredentials) -> Self {
        BrokerResponse {
            decision: "allow".into(),
            reason: None,
            token: None,
            username: None,
            aws_access_key_id: creds.access_key_id,
            aws_secret_access_key: creds.secret_access_key,
            aws_session_token: creds.session_token,
            aws_profile: creds.profile_name,
            aws_region: Some(creds.region),
        }
    }
}

/// Resolves the right `Approver` for a request at serve time. The singleton
/// broker serves many runs from one socket, so it cannot hold a single
/// run-bound approver; instead it asks this resolver, per request, for the
/// approver of `run_id`. Returning `None` means "no live run to approve
/// against" → the caller fail-closed denies any `Ask`.
#[async_trait]
pub trait ApproverResolver: Send + Sync {
    async fn resolve(&self, run_id: Option<&str>) -> Option<Arc<dyn Approver>>;
}

/// Config for the app-wide singleton broker.
pub struct BrokerConfig {
    /// Label used for the socket file name (`<label>.sock`). One stable label
    /// ("app") gives one long-lived socket shared by every run.
    pub socket_label: String,
    /// Per-request approver resolver (see `ApproverResolver`).
    pub resolver: Arc<dyn ApproverResolver>,
}

/// Handle to a running broker. Dropping it aborts the accept loop and removes the
/// socket file (best-effort — never panics if the file is already gone).
pub struct BrokerHandle {
    pub path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl BrokerHandle {
    /// Explicit stop: abort the loop and remove the socket file.
    #[allow(dead_code)] // used once the broker is wired into the run path (GĐ3)
    pub fn stop(self) {
        // Dropping runs the same cleanup.
        drop(self);
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn socket_path(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join("devdy-broker")
        .join(format!("{label}.sock"))
}

/// Bind the app-wide Unix socket, spawn the accept loop, and return a handle.
/// The socket path is `BrokerHandle.path`; pass it to every run's shim via env.
/// Lives for the whole app lifetime (dropped only on app exit), so runs never
/// race a per-run socket that appears/disappears with them.
pub async fn start_broker(db: Db, cfg: BrokerConfig) -> Result<BrokerHandle, String> {
    let path = socket_path(&cfg.socket_label);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Remove any stale socket (e.g. from a previous app run) to avoid AddrInUse.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;

    // Best-effort: restrict the socket to the owner.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perm = meta.permissions();
            perm.set_mode(0o600);
            let _ = std::fs::set_permissions(&path, perm);
        }
    }

    let resolver = cfg.resolver.clone();
    let db_clone = db.clone();

    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let db = db_clone.clone();
                    let resolver = resolver.clone();
                    // One task per connection; a bad client must not kill the loop.
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, &db, resolver).await {
                            tracing::warn!(target: "devdy::broker", "connection error: {e}");
                        }
                    });
                }
                Err(e) => {
                    // Transient accept error; log and continue — never panic.
                    tracing::warn!(target: "devdy::broker", "accept error: {e}");
                }
            }
        }
    });

    Ok(BrokerHandle { path, task })
}

/// Read NDJSON requests from one connection and reply to each. Loops until EOF.
async fn handle_connection(
    stream: UnixStream,
    db: &Db,
    resolver: Arc<dyn ApproverResolver>,
) -> Result<(), String> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = process_line(&line, db, resolver.as_ref()).await;
        let mut out = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
        out.push('\n');
        write_half
            .write_all(out.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        write_half.flush().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Process a single request line into a response, auditing along the way.
/// Never returns a token in any log or error path.
async fn process_line(line: &str, db: &Db, resolver: &dyn ApproverResolver) -> BrokerResponse {
    // 1. Parse — malformed → deny (fail-closed).
    let req: BrokerRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(_) => {
            audit::audit_request("?", "?", &[], "deny", Some("malformed request"));
            return BrokerResponse::deny("malformed request");
        }
    };

    // Audit under the run that issued the request (falls back to "?").
    let run_id = req.run_id.as_deref().unwrap_or("?");

    // 2. Pure policy.
    let decision = policy::evaluate_policy(&req.tool, &req.argv);

    // 3. Resolve decision → whether we proceed to token resolution.
    let (proceed, base_resp): (bool, Option<BrokerResponse>) = match &decision {
        PolicyDecision::Deny { reason } => {
            audit::audit_request(run_id, &req.tool, &req.argv, "deny", Some(reason));
            (false, Some(BrokerResponse::deny(reason.clone())))
        }
        PolicyDecision::Ask { reason } => {
            // Route the Ask to the issuing run's approver. No live run (ended, or
            // no run_id) → fail-closed deny; we never auto-approve a write.
            let approved = match resolver.resolve(req.run_id.as_deref()).await {
                Some(approver) => approver.approve(&req, reason).await,
                None => false,
            };
            if approved {
                // Approved Ask → proceed like Allow.
                (true, None)
            } else {
                // Rejected / no live run → fail-closed deny (plan §3.3 step 3).
                audit::audit_request(run_id, &req.tool, &req.argv, "deny", Some(reason));
                (false, Some(BrokerResponse::deny(reason.clone())))
            }
        }
        PolicyDecision::Allow => (true, None),
    };

    if !proceed {
        // base_resp is always Some here.
        return base_resp.unwrap_or_else(|| BrokerResponse::deny("denied"));
    }

    if req.tool == "aws" {
        return match token::resolve_aws_credentials_for_project(db, &req.project_id).await {
            Ok(Some(resolved)) => {
                audit::audit_request(run_id, &req.tool, &req.argv, "allow", None);
                BrokerResponse::allow_aws(resolved)
            }
            Ok(None) => {
                let reason = "no AWS account linked for this project or credential unavailable";
                audit::audit_request(run_id, &req.tool, &req.argv, "deny", Some(reason));
                BrokerResponse::deny(reason)
            }
            Err(_) => {
                let reason = "AWS credential resolve error";
                audit::audit_request(run_id, &req.tool, &req.argv, "deny", Some(reason));
                BrokerResponse::deny(reason)
            }
        };
    }

    // Is this a `git credential get` request? Only then do we return a username
    // for the git credential helper (with a provider-appropriate fallback).
    let is_credential = req.tool == "git"
        && req
            .argv
            .first()
            .map(|a| a.eq_ignore_ascii_case("credential"))
            .unwrap_or(false);

    // 4. Token resolution (fail-closed).
    match token::resolve_token_for_project(db, &req.project_id, &req.tool, req.host.as_deref())
        .await
    {
        Ok(Some(resolved)) => {
            audit::audit_request(run_id, &req.tool, &req.argv, "allow", None);
            if is_credential {
                // username = real account username if present; else a provider
                // convention: github → `x-access-token`, gitlab → `oauth2`.
                let username = resolved
                    .username
                    .clone()
                    .unwrap_or_else(|| credential_username_fallback(&resolved.host));
                BrokerResponse::allow_credential(resolved.token, username)
            } else {
                BrokerResponse::allow(Some(resolved.token))
            }
        }
        Ok(None) => {
            // Allow policy but no token → fail-closed deny (plan §3.3 / OQ#3).
            let reason = "no account linked for this project/host";
            audit::audit_request(run_id, &req.tool, &req.argv, "deny", Some(reason));
            BrokerResponse::deny(reason)
        }
        Err(_) => {
            // Never surface the underlying error (may reference secrets).
            let reason = "token resolve error";
            audit::audit_request(run_id, &req.tool, &req.argv, "deny", Some(reason));
            BrokerResponse::deny(reason)
        }
    }
}

/// Provider-appropriate git credential username when the account has none stored.
/// GitHub accepts any username with a PAT as password; the safe convention is
/// `x-access-token`. GitLab accepts `oauth2` + PAT as password. Gitlab is
/// detected by host (not github.com / api.github.com); everything else defaults
/// to the GitHub convention.
fn credential_username_fallback(host: &str) -> String {
    let h = host.trim().to_ascii_lowercase();
    if h == "github.com" || h == "api.github.com" || h.ends_with(".github.com") {
        "x-access-token".to_string()
    } else {
        "oauth2".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::approver::{Approver, DenyAllApprover, FixedApprover};
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Test resolver: always returns the same approver regardless of `run_id`,
    /// so the existing round-trip tests exercise the approver branch directly.
    struct StaticResolver(Arc<dyn Approver>);

    #[async_trait]
    impl ApproverResolver for StaticResolver {
        async fn resolve(&self, _run_id: Option<&str>) -> Option<Arc<dyn Approver>> {
            Some(self.0.clone())
        }
    }

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY, github_account_id TEXT, gitlab_account_id TEXT, aws_account_id TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE github_accounts (id TEXT PRIMARY KEY, label TEXT, username TEXT, email TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE gitlab_accounts (id TEXT PRIMARY KEY, label TEXT, host TEXT, username TEXT, email TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE aws_accounts (id TEXT PRIMARY KEY, label TEXT, auth_method TEXT, account_id TEXT, arn TEXT, region TEXT, access_key_id TEXT, profile_name TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn round_trip(db: Db, approver: Arc<dyn Approver>, req_json: &str) -> BrokerResponse {
        let socket_label = uuid::Uuid::new_v4().to_string();
        let resolver: Arc<dyn ApproverResolver> = Arc::new(StaticResolver(approver));
        let handle = start_broker(
            db,
            BrokerConfig {
                socket_label,
                resolver,
            },
        )
        .await
        .unwrap();
        let stream = UnixStream::connect(&handle.path).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut line = req_json.to_string();
        line.push('\n');
        w.write_all(line.as_bytes()).await.unwrap();
        w.flush().await.unwrap();
        let mut lines = BufReader::new(r).lines();
        let resp_line = lines.next_line().await.unwrap().unwrap();
        serde_json::from_str::<serde_json::Value>(&resp_line).unwrap();
        // parse into typed for asserts
        let v: serde_json::Value = serde_json::from_str(&resp_line).unwrap();
        BrokerResponse {
            decision: v["decision"].as_str().unwrap().to_string(),
            reason: v
                .get("reason")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            token: v
                .get("token")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            username: v
                .get("username")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            aws_access_key_id: v
                .get("aws_access_key_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            aws_secret_access_key: v
                .get("aws_secret_access_key")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            aws_session_token: v
                .get("aws_session_token")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            aws_profile: v
                .get("aws_profile")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            aws_region: v
                .get("aws_region")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        }
    }

    #[tokio::test]
    async fn round_trip_deny_on_denylist() {
        let db = mem_db().await;
        let resp = round_trip(
            db,
            Arc::new(DenyAllApprover),
            r#"{"project_id":"p1","tool":"gh","argv":["auth","token"]}"#,
        )
        .await;
        assert_eq!(resp.decision, "deny");
        assert!(resp.token.is_none());
    }

    #[tokio::test]
    async fn round_trip_ask_auto_denied() {
        let db = mem_db().await;
        // gh pr create → Ask; FixedApprover(false) → deny (no token).
        let resp = round_trip(
            db,
            Arc::new(FixedApprover(false)),
            r#"{"project_id":"p1","tool":"gh","argv":["pr","create"]}"#,
        )
        .await;
        assert_eq!(resp.decision, "deny");
        assert!(resp.token.is_none());
    }

    #[tokio::test]
    async fn round_trip_ask_no_live_run_is_deny() {
        // Resolver returns None (run ended / no run_id) → Ask must fail-closed
        // deny WITHOUT consulting any approver. This is the singleton-broker
        // guarantee: a write issued outside a live run is never auto-approved.
        struct NoneResolver;
        #[async_trait]
        impl ApproverResolver for NoneResolver {
            async fn resolve(&self, _run_id: Option<&str>) -> Option<Arc<dyn Approver>> {
                None
            }
        }
        let db = mem_db().await;
        let socket_label = uuid::Uuid::new_v4().to_string();
        let resolver: Arc<dyn ApproverResolver> = Arc::new(NoneResolver);
        let handle = start_broker(
            db,
            BrokerConfig {
                socket_label,
                resolver,
            },
        )
        .await
        .unwrap();
        let stream = UnixStream::connect(&handle.path).await.unwrap();
        let (r, mut w) = stream.into_split();
        w.write_all(b"{\"project_id\":\"p1\",\"tool\":\"gh\",\"argv\":[\"issue\",\"comment\"]}\n")
            .await
            .unwrap();
        w.flush().await.unwrap();
        let mut lines = BufReader::new(r).lines();
        let resp_line = lines.next_line().await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp_line).unwrap();
        assert_eq!(v["decision"].as_str().unwrap(), "deny");
        assert!(v.get("token").and_then(|x| x.as_str()).is_none());
    }

    #[tokio::test]
    async fn round_trip_allow_but_no_token_is_deny() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id) VALUES ('p1')")
            .execute(&db)
            .await
            .unwrap();
        // gh pr list → Allow policy, but no account linked → fail-closed deny.
        let resp = round_trip(
            db,
            Arc::new(DenyAllApprover),
            r#"{"project_id":"p1","tool":"gh","argv":["pr","list"]}"#,
        )
        .await;
        assert_eq!(resp.decision, "deny");
        assert!(resp.token.is_none());
    }

    #[tokio::test]
    async fn round_trip_aws_profile_allow_returns_profile_fields() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO aws_accounts (id, label, auth_method, account_id, arn, region, profile_name) VALUES ('aws1','Work','profile','123456789012','arn:aws:iam::123456789012:user/dev','ap-northeast-1','work-sso')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects (id, aws_account_id) VALUES ('p1','aws1')")
            .execute(&db)
            .await
            .unwrap();
        let resp = round_trip(
            db,
            Arc::new(DenyAllApprover),
            r#"{"project_id":"p1","tool":"aws","argv":["sts","get-caller-identity"]}"#,
        )
        .await;
        assert_eq!(resp.decision, "allow");
        assert_eq!(resp.aws_profile.as_deref(), Some("work-sso"));
        assert_eq!(resp.aws_region.as_deref(), Some("ap-northeast-1"));
        assert!(resp.token.is_none());
    }

    #[tokio::test]
    async fn round_trip_malformed_is_deny() {
        let db = mem_db().await;
        let resp = round_trip(db, Arc::new(DenyAllApprover), "not json").await;
        assert_eq!(resp.decision, "deny");
    }

    // ---- AC1/AC4: git credential get branch ----

    #[test]
    fn credential_username_fallback_by_provider() {
        assert_eq!(credential_username_fallback("github.com"), "x-access-token");
        assert_eq!(
            credential_username_fallback("api.github.com"),
            "x-access-token"
        );
        assert_eq!(credential_username_fallback("gitlab.com"), "oauth2");
        assert_eq!(credential_username_fallback("gitlab.example.com"), "oauth2");
    }

    #[tokio::test]
    async fn credential_get_no_account_is_deny_fail_closed() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id) VALUES ('p1')")
            .execute(&db)
            .await
            .unwrap();
        // `credential get` is Allow by policy, but no linked account → fail-closed
        // deny (helper then prints nothing → git auth fails, run survives).
        let resp = round_trip(
            db,
            Arc::new(DenyAllApprover),
            r#"{"project_id":"p1","tool":"git","argv":["credential","get"],"host":"github.com"}"#,
        )
        .await;
        assert_eq!(resp.decision, "deny");
        assert!(resp.token.is_none());
        assert!(resp.username.is_none());
    }

    #[tokio::test]
    async fn credential_get_spoofed_host_is_deny() {
        let db = mem_db().await;
        // Link a github account so a matching host would resolve — but spoofed
        // host must be rejected at resolve_git (host allowlist), yielding deny.
        sqlx::query(
            "INSERT INTO github_accounts (id, label, username) VALUES ('gh1','work','ghuser')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO projects (id, github_account_id) VALUES ('p1','gh1')")
            .execute(&db)
            .await
            .unwrap();
        let resp = round_trip(
            db,
            Arc::new(DenyAllApprover),
            r#"{"project_id":"p1","tool":"git","argv":["credential","get"],"host":"github.attacker.io"}"#,
        )
        .await;
        assert_eq!(resp.decision, "deny");
        assert!(resp.token.is_none());
    }

    #[tokio::test]
    async fn socket_cleaned_up_on_drop() {
        let db = mem_db().await;
        let socket_label = uuid::Uuid::new_v4().to_string();
        let resolver: Arc<dyn ApproverResolver> =
            Arc::new(StaticResolver(Arc::new(DenyAllApprover)));
        let handle = start_broker(
            db,
            BrokerConfig {
                socket_label,
                resolver,
            },
        )
        .await
        .unwrap();
        let path = handle.path.clone();
        assert!(path.exists());
        drop(handle);
        // Give the abort + remove a moment.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!path.exists(), "socket file should be removed on drop");
    }
}
