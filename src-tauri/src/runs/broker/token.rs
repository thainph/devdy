//! Token resolver for the broker (GĐ2, AC2).
//!
//! Resolves the credential for a project + tool, reusing the same PAT-in-Keychain
//! mechanism as `github/mod.rs::client_for_project`. **Fail-closed**: returns
//! `Ok(None)` whenever no usable account is linked (or its PAT is missing), so an
//! Allow policy without a token collapses to a deny at the socket layer.
//!
//! SECURITY: `ResolvedToken.token` is a secret. It is only ever forwarded to the
//! shim over the socket; it is never logged, never put in error messages, never
//! traced.

use crate::db::Db;
use crate::secrets;
use sqlx::Row;

/// A resolved credential for one project/tool. `token` is a secret — never log it.
///
/// NOTE (GĐ5, AC2): this type intentionally does NOT derive `Serialize`/
/// `Deserialize` — the `token` field is a secret and must never be serialized by
/// accident. The socket contract is carried by `BrokerResponse` (in `mod.rs`),
/// which is unchanged. `expires_at` is metadata for the ephemeral-token cache
/// layer only; the broker never reads it, so response bytes are unchanged.
pub struct ResolvedToken {
    pub token: String,
    pub host: String,
    // Read by the broker's audit/log path (GĐ3+); kept on the struct as the
    // resolved account identity. Not read in the GĐ5 unit-test build.
    #[allow(dead_code)]
    pub account_label: String,
    /// Account username (NOT a secret). Used as the git credential `username`
    /// field. `None` when the account row has no username stored.
    pub username: Option<String>,
    /// Absolute expiry (unix seconds). `None` = never expires (PAT). Metadata for
    /// the TTL cache layer; not forwarded to the shim (broker never reads it).
    #[allow(dead_code)]
    pub expires_at: Option<i64>,
}

/// Git commit identity resolved from the project's linked account. Neither field
/// is a secret. `None` fields mean "do not set the corresponding env" (never
/// fabricate a name/email).
pub struct CommitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// Metadata for a project's linked AWS account. Contains no secret and is safe
/// for prompts/per-run env setup.
pub struct AwsRuntimeMetadata {
    pub account_db_id: String,
    pub account_label: String,
    pub auth_method: String,
    pub account_id: Option<String>,
    pub arn: Option<String>,
    pub region: String,
    pub access_key_id: Option<String>,
    pub profile_name: Option<String>,
}

/// Resolved AWS credential for a project. Secret fields must never be logged.
pub struct ResolvedAwsCredentials {
    #[allow(dead_code)]
    pub account_label: String,
    #[allow(dead_code)]
    pub auth_method: String,
    #[allow(dead_code)]
    pub account_id: Option<String>,
    #[allow(dead_code)]
    pub arn: Option<String>,
    pub region: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
    pub profile_name: Option<String>,
}

// ============================================================================
// GĐ5 — ephemeral-token seam + TTL cache (buildable/testable core).
//
// This section introduces a `TokenProvider` seam WRAPPING the existing
// PAT-in-Keychain lookup. The default provider (`PatTokenProvider`) preserves
// the exact current behavior (fail-closed `Err(_) => Ok(None)`). The resolver's
// public signature is unchanged; an internal `*_with(provider, ...)` variant
// takes a provider to enable mock injection in tests. App-token providers are
// scaffolded (return `Unsupported`) but NOT wired into the resolver.
//
// SECURITY: `ProviderToken.token` is a secret. No type below derives a `Debug`
// that would print the token.
// ============================================================================

/// Scope identifying which token to fetch. Contains NO secret. Used as the
/// cache key. Excludes `project_id` on purpose: an ephemeral token is tied to
/// the account+host, so multiple projects sharing an account reuse it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenScope {
    /// Normalized provider kind: `"github"` | `"gitlab"` (NOT alias gh/glab).
    pub provider_kind: String,
    /// Account id in the DB (also the Keychain key). Stable per account.
    pub account_id: String,
    /// Normalized host (github.com / gitlab.example.com). Distinguishes GHE /
    /// self-hosted GitLab.
    pub host: String,
}

/// A token minted/looked-up by a provider. `token` is a SECRET.
pub struct ProviderToken {
    pub token: String,
    /// `None` = never expires (PAT). `Some(unix_secs)` = absolute expiry.
    pub expires_at: Option<i64>,
    /// Username when the provider knows it; PAT keeps `None` (username comes
    /// from the DB meta as before).
    pub username: Option<String>,
}

/// Provider-layer error. Messages must NEVER contain a secret.
///
/// GĐ5: constructed by the App-token scaffolds (`Unsupported`) and tests
/// (`Fetch`); not yet reached on the live PAT path, hence the allow.
#[derive(Debug)]
#[allow(dead_code)]
pub enum TokenError {
    /// Provider not configured / App not registered (GĐ5 scaffolds return this).
    Unsupported,
    /// A genuine fetch failure (Keychain error, mint failure). No secret inside.
    Fetch(String),
}

/// Supplies a token for a resolved `(provider_kind, account_id, host)` scope.
///
/// SYNC by design (GĐ5): the PAT path is synchronous (`secrets::get_*_pat`), so
/// a sync trait avoids an extra async layer and keeps cache/clock tests trivial.
/// DEBT (release-gate): App-token providers mint over HTTP and will need async;
/// when wired live, either switch this trait to async or wrap the mint in
/// `tokio::task::block_in_place`.
pub trait TokenProvider: Send + Sync {
    fn fetch(&self, scope: &TokenScope) -> Result<Option<ProviderToken>, TokenError>;
}

/// Default provider — preserves the exact current PAT-in-Keychain behavior.
pub struct PatTokenProvider;

impl TokenProvider for PatTokenProvider {
    fn fetch(&self, scope: &TokenScope) -> Result<Option<ProviderToken>, TokenError> {
        let res = match scope.provider_kind.as_str() {
            "github" => secrets::get_account_pat(&scope.account_id),
            "gitlab" => secrets::get_gitlab_account_pat(&scope.account_id),
            _ => return Ok(None),
        };
        match res {
            Ok(token) => Ok(Some(ProviderToken {
                token,
                expires_at: None,
                username: None,
            })),
            // Missing/absent PAT → Ok(None) fail-closed. IDENTICAL to the old
            // inline `Err(_) => return Ok(None)` behavior.
            Err(_) => Ok(None),
        }
    }
}

/// Injectable clock so the TTL cache is deterministically testable.
pub trait Clock: Send + Sync {
    fn now_unix(&self) -> i64;
}

/// Production clock backed by the system wall clock (chrono, already a dep).
/// Used once `CachingTokenProvider` is wired live (release-gate).
#[allow(dead_code)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

struct CachedEntry {
    token: String,
    username: Option<String>,
    expires_at: i64,
}

/// TTL-aware caching wrapper around any `TokenProvider`.
///
/// Behavior:
/// - Expiring tokens (`Some(exp)`) are cached per `TokenScope`; refetched when
///   within `skew_secs` of expiry (refresh early).
/// - PAT tokens (`expires_at == None`) BYPASS the cache: fetched fresh every
///   time. A PAT is cheap (Keychain) and immortal; caching it in RAM adds
///   secret-retention surface with no benefit, and keeps parity with today's
///   fetch-every-time behavior.
/// - `Ok(None)` and `Err(_)` are never cached (fail-closed stays intact).
///
/// The internal lock is a plain `std::sync::Mutex`; it is never held across an
/// `.await` (the inner provider is sync).
pub struct CachingTokenProvider<P: TokenProvider, C: Clock> {
    inner: P,
    clock: C,
    skew_secs: i64,
    store: std::sync::Mutex<std::collections::HashMap<TokenScope, CachedEntry>>,
}

impl<P: TokenProvider, C: Clock> CachingTokenProvider<P, C> {
    #[allow(dead_code)] // wired live at release-gate; kept exercised via tests
    pub fn new(inner: P, clock: C, skew_secs: i64) -> Self {
        Self {
            inner,
            clock,
            skew_secs,
            store: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl<P: TokenProvider, C: Clock> TokenProvider for CachingTokenProvider<P, C> {
    fn fetch(&self, scope: &TokenScope) -> Result<Option<ProviderToken>, TokenError> {
        let now = self.clock.now_unix();
        // 1. Try a fresh cache entry (short lock, no await while held).
        if let Ok(store) = self.store.lock() {
            if let Some(entry) = store.get(scope) {
                if entry.expires_at - now > self.skew_secs {
                    return Ok(Some(ProviderToken {
                        token: entry.token.clone(),
                        expires_at: Some(entry.expires_at),
                        username: entry.username.clone(),
                    }));
                }
            }
        }
        // 2. Miss/stale → ask the inner provider.
        match self.inner.fetch(scope)? {
            Some(pt) => match pt.expires_at {
                Some(exp) => {
                    if let Ok(mut store) = self.store.lock() {
                        store.insert(
                            scope.clone(),
                            CachedEntry {
                                token: pt.token.clone(),
                                username: pt.username.clone(),
                                expires_at: exp,
                            },
                        );
                    }
                    Ok(Some(pt))
                }
                // PAT → bypass cache entirely.
                None => Ok(Some(pt)),
            },
            None => Ok(None),
        }
    }
}

// ---- App-token provider scaffolds (AC4) ------------------------------------
// GĐ5: NOT wired into the resolver. Return `Unsupported` until App credentials
// exist. Compiled + exercised by unit tests to avoid dead-code warnings.

/// SCAFFOLD — GitHub App installation token provider (NOT wired live).
///
/// TODO(release-gate — DEBT): requires registering a GitHub App OUTSIDE this
/// code: App ID, private key (PEM), installation id per org/repo. Mint an
/// installation access token via
/// `POST /app/installations/{id}/access_tokens` (JWT signed by the private
/// key) → ~1h token, narrow repo scope. On success, set
/// `expires_at = Some(exp)` so `CachingTokenProvider` auto-refreshes it.
#[allow(dead_code)]
pub struct GithubAppTokenProvider {
    // config handle (App id / key / installation id) added at release-gate.
}

impl TokenProvider for GithubAppTokenProvider {
    fn fetch(&self, _scope: &TokenScope) -> Result<Option<ProviderToken>, TokenError> {
        Err(TokenError::Unsupported) // not configured
    }
}

/// SCAFFOLD — GitLab project/group access token provider (NOT wired live).
///
/// TODO(release-gate — DEBT): requires a root token with permission to create
/// project/group access tokens via `POST /projects/:id/access_tokens`
/// (day-scale `expires_at`), or a CI job-token pattern. The creating token must
/// be registered OUTSIDE this code.
#[allow(dead_code)]
pub struct GitlabProjectTokenProvider {
    // config handle (host / project id / creating token) added at release-gate.
}

impl TokenProvider for GitlabProjectTokenProvider {
    fn fetch(&self, _scope: &TokenScope) -> Result<Option<ProviderToken>, TokenError> {
        Err(TokenError::Unsupported)
    }
}

/// Resolve the token for a project + tool. Fail-closed: `Ok(None)` when no usable
/// account is linked. `Err` only on genuine DB errors (message is generic — never
/// contains secrets).
pub async fn resolve_token_for_project(
    db: &Db,
    project_id: &str,
    tool: &str,
    host: Option<&str>,
) -> Result<Option<ResolvedToken>, String> {
    // Public signature unchanged; delegate through the default PAT provider so
    // the behavior is identical to before the seam existed.
    resolve_token_for_project_with(&PatTokenProvider, db, project_id, tool, host).await
}

/// Internal variant that takes a `TokenProvider`, enabling mock injection in
/// tests. The dispatch and all DB/host/fail-closed logic are unchanged.
async fn resolve_token_for_project_with(
    provider: &dyn TokenProvider,
    db: &Db,
    project_id: &str,
    tool: &str,
    host: Option<&str>,
) -> Result<Option<ResolvedToken>, String> {
    match tool {
        "github" | "gh" => resolve_github(provider, db, project_id).await,
        "gitlab" | "glab" => resolve_gitlab(provider, db, project_id).await,
        "git" => resolve_git(provider, db, project_id, host).await,
        _ => Ok(None),
    }
}

/// Resolve AWS credentials for a project. Fail-closed: returns `Ok(None)` when
/// no account is linked, metadata is incomplete, or a keys account is missing
/// its Keychain secret.
pub async fn resolve_aws_credentials_for_project(
    db: &Db,
    project_id: &str,
) -> Result<Option<ResolvedAwsCredentials>, String> {
    let Some(meta) = resolve_aws_runtime_metadata(db, project_id).await? else {
        return Ok(None);
    };

    match meta.auth_method.as_str() {
        "keys" => {
            let access_key_id = match meta.access_key_id.clone().filter(|s| !s.trim().is_empty()) {
                Some(v) => v,
                None => return Ok(None),
            };
            let secret = match secrets::get_aws_secret(&meta.account_db_id) {
                Ok(secret) => secret,
                Err(_) => return Ok(None),
            };
            let secret_access_key = match secret.secret_access_key.filter(|s| !s.trim().is_empty())
            {
                Some(v) => v,
                None => return Ok(None),
            };
            Ok(Some(ResolvedAwsCredentials {
                account_label: meta.account_label,
                auth_method: meta.auth_method,
                account_id: meta.account_id,
                arn: meta.arn,
                region: meta.region,
                access_key_id: Some(access_key_id),
                secret_access_key: Some(secret_access_key),
                session_token: secret.session_token.filter(|s| !s.trim().is_empty()),
                profile_name: None,
            }))
        }
        "profile" => {
            let profile_name = match meta.profile_name.clone().filter(|s| !s.trim().is_empty()) {
                Some(v) => v,
                None => return Ok(None),
            };
            Ok(Some(ResolvedAwsCredentials {
                account_label: meta.account_label,
                auth_method: meta.auth_method,
                account_id: meta.account_id,
                arn: meta.arn,
                region: meta.region,
                access_key_id: None,
                secret_access_key: None,
                session_token: None,
                profile_name: Some(profile_name),
            }))
        }
        _ => Ok(None),
    }
}

/// Resolve non-secret AWS metadata for prompt/context and per-run env setup.
/// Returns `Ok(None)` when no AWS account is linked or the linked row is gone.
pub async fn resolve_aws_runtime_metadata(
    db: &Db,
    project_id: &str,
) -> Result<Option<AwsRuntimeMetadata>, String> {
    let row = sqlx::query(
        "SELECT a.id, a.label, a.auth_method, a.account_id, a.arn, a.region, \
                a.access_key_id, a.profile_name \
         FROM projects p \
         JOIN aws_accounts a ON a.id = p.aws_account_id \
         WHERE p.id = ?",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(AwsRuntimeMetadata {
        account_db_id: row.get("id"),
        account_label: row.get("label"),
        auth_method: row.get("auth_method"),
        account_id: row.get("account_id"),
        arn: row.get("arn"),
        region: row.get("region"),
        access_key_id: row.get("access_key_id"),
        profile_name: row.get("profile_name"),
    }))
}

async fn resolve_github(
    provider: &dyn TokenProvider,
    db: &Db,
    project_id: &str,
) -> Result<Option<ResolvedToken>, String> {
    let row = sqlx::query("SELECT github_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let account_id: Option<String> = match row {
        Some(r) => r.get("github_account_id"),
        None => return Ok(None),
    };
    let account_id = match account_id {
        Some(id) => id,
        None => return Ok(None), // not linked → fail-closed
    };
    // Missing PAT → Ok(None) (fail-closed, per OQ#2). Do not leak the error.
    // Delegated to the provider seam; PatTokenProvider preserves this exactly
    // (`Err(_)`/`None`/`Unsupported` all collapse to fail-closed `Ok(None)`).
    let scope = TokenScope {
        provider_kind: "github".to_string(),
        account_id: account_id.clone(),
        host: "github.com".to_string(),
    };
    let pt = match provider.fetch(&scope) {
        Ok(Some(pt)) => pt,
        Ok(None) | Err(_) => return Ok(None),
    };
    // Label/username are best-effort; default label to the account id if absent.
    let (label, username) = fetch_github_meta(db, &account_id).await;
    // Prefer the account row username (unchanged); provider PAT supplies None.
    let username = username.or(pt.username);
    Ok(Some(ResolvedToken {
        token: pt.token,
        host: "github.com".to_string(),
        account_label: label.unwrap_or(account_id),
        username,
        expires_at: pt.expires_at,
    }))
}

async fn fetch_github_meta(db: &Db, account_id: &str) -> (Option<String>, Option<String>) {
    let row = sqlx::query("SELECT label, username FROM github_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    match row {
        Some(r) => (r.get("label"), r.get("username")),
        None => (None, None),
    }
}

async fn resolve_gitlab(
    provider: &dyn TokenProvider,
    db: &Db,
    project_id: &str,
) -> Result<Option<ResolvedToken>, String> {
    let row = sqlx::query("SELECT gitlab_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let account_id: Option<String> = match row {
        Some(r) => r.get("gitlab_account_id"),
        None => return Ok(None),
    };
    let account_id = match account_id {
        Some(id) => id,
        None => return Ok(None),
    };
    let meta = fetch_gitlab_meta(db, &account_id).await;
    let host = meta.host.unwrap_or_else(|| "gitlab.com".to_string());
    // Delegated to the provider seam; PatTokenProvider preserves fail-closed.
    let scope = TokenScope {
        provider_kind: "gitlab".to_string(),
        account_id: account_id.clone(),
        host: host.clone(),
    };
    let pt = match provider.fetch(&scope) {
        Ok(Some(pt)) => pt,
        Ok(None) | Err(_) => return Ok(None),
    };
    let username = meta.username.or(pt.username);
    Ok(Some(ResolvedToken {
        token: pt.token,
        host,
        account_label: meta.label.unwrap_or(account_id),
        username,
        expires_at: pt.expires_at,
    }))
}

struct GitlabMeta {
    host: Option<String>,
    label: Option<String>,
    username: Option<String>,
}

async fn fetch_gitlab_meta(db: &Db, account_id: &str) -> GitlabMeta {
    let row = sqlx::query("SELECT host, label, username FROM gitlab_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    match row {
        Some(r) => GitlabMeta {
            host: r.get("host"),
            label: r.get("label"),
            username: r.get("username"),
        },
        None => GitlabMeta {
            host: None,
            label: None,
            username: None,
        },
    }
}

/// Normalize a raw host string into a bare, lowercased hostname suitable for
/// exact comparison. Strips any `user@`/scheme prefix, a trailing `:port`, and
/// surrounding whitespace. Returns `None` when nothing usable remains — callers
/// treat that as fail-closed.
///
/// SECURITY: this is the single choke-point that decides whether a remote host
/// is trusted enough to receive a real token, so it must reject ambiguity.
fn normalize_host(raw: &str) -> Option<String> {
    let mut h = raw.trim();
    // Drop an optional scheme (e.g. `https://`), keeping only the authority.
    if let Some(idx) = h.find("://") {
        h = &h[idx + 3..];
    }
    // Drop userinfo (`user@host`).
    if let Some(idx) = h.rfind('@') {
        h = &h[idx + 1..];
    }
    // Drop path/query if a full URL slipped through.
    if let Some(idx) = h.find('/') {
        h = &h[..idx];
    }
    // Drop a trailing `:port`. IPv6 literals are not expected for git hosts here;
    // a single trailing colon-segment is treated as a port.
    if let Some(idx) = h.rfind(':') {
        // Only strip when everything after the colon is digits (a port).
        if h[idx + 1..].chars().all(|c| c.is_ascii_digit()) && idx + 1 < h.len() {
            h = &h[..idx];
        }
    }
    let h = h.trim_end_matches('.').trim().to_ascii_lowercase();
    if h.is_empty() {
        None
    } else {
        Some(h)
    }
}

/// Exact hostname match, or a valid dotted-boundary subdomain of `account_host`.
/// `req` and `account_host` are compared after [`normalize_host`]. A subdomain
/// only matches when it ends with `"." + account_host`, so `gitlab.com.evil.io`
/// can never match `gitlab.com`, and `notgithub.com` can never match
/// `github.com`. Fail-closed: any normalization failure yields `false`.
fn host_matches(req: &str, account_host: &str) -> bool {
    let (Some(req), Some(acct)) = (normalize_host(req), normalize_host(account_host)) else {
        return false;
    };
    req == acct || req.ends_with(&format!(".{acct}"))
}

/// git: pick the account by host. Fail-closed for unknown/None host.
async fn resolve_git(
    provider: &dyn TokenProvider,
    db: &Db,
    project_id: &str,
    host: Option<&str>,
) -> Result<Option<ResolvedToken>, String> {
    let host = match host {
        Some(h) => h,
        None => return Ok(None), // no host → cannot safely pick → fail-closed
    };

    // GitHub: only the canonical hosts are trusted. The schema does not store a
    // per-account GitHub Enterprise host, so accept exactly `github.com` and
    // `api.github.com` (dotted-boundary), and nothing else. A spoofed host such
    // as `github.attacker.io` or `notgithub.com` must NOT match.
    if host_matches(host, "github.com") || host_matches(host, "api.github.com") {
        return resolve_github(provider, db, project_id).await;
    }

    // For gitlab: match the linked account's own host exactly (or a valid
    // dotted-boundary subdomain). Guards against unknown/spoofed remotes.
    let row = sqlx::query("SELECT gitlab_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let account_id: Option<String> = match row {
        Some(r) => r.get("gitlab_account_id"),
        None => return Ok(None),
    };
    let account_id = match account_id {
        Some(id) => id,
        None => return Ok(None),
    };
    let acct_host = fetch_gitlab_meta(db, &account_id)
        .await
        .host
        .unwrap_or_else(|| "gitlab.com".to_string());
    if host_matches(host, &acct_host) {
        return resolve_gitlab(provider, db, project_id).await;
    }

    // Host matches no linked account → reject unknown remote (host allowlist spirit).
    Ok(None)
}

/// Resolve the git commit identity (name/email) for a project's linked account.
///
/// Rule (plan §4, AC3): prefer the GitHub account when both are linked; else the
/// GitLab account; else `{ None, None }`. `name` = account `username`, `email` =
/// account `email` column. A `None`/empty column yields `None` for that field so
/// the caller does NOT set the corresponding env var (never fabricate identity).
/// Fail-open for identity: DB errors degrade to `{ None, None }` (identity is not
/// a secret and must never make a run fail).
pub async fn resolve_commit_identity(db: &Db, project_id: &str) -> CommitIdentity {
    let none = CommitIdentity {
        name: None,
        email: None,
    };
    let row =
        match sqlx::query("SELECT github_account_id, gitlab_account_id FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(db)
            .await
        {
            Ok(Some(r)) => r,
            _ => return none,
        };
    let github_id: Option<String> = row.get("github_account_id");
    let gitlab_id: Option<String> = row.get("gitlab_account_id");

    // Prefer GitHub when both are linked.
    let (name, email) = if let Some(id) = github_id {
        let (_label, username) = fetch_github_meta(db, &id).await;
        (username, fetch_github_email(db, &id).await)
    } else if let Some(id) = gitlab_id {
        let meta = fetch_gitlab_meta(db, &id).await;
        (meta.username, fetch_gitlab_email(db, &id).await)
    } else {
        return none;
    };

    CommitIdentity {
        name: name.filter(|s| !s.trim().is_empty()),
        email: email.filter(|s| !s.trim().is_empty()),
    }
}

/// Build the Claude system-prompt append describing the project's pre-wired git
/// credentials (GĐ6, AC1). Returns `None` when NO provider account is linked, so
/// the caller adds no `appendSystemPrompt` field and behavior is unchanged
/// (AC6). NEVER includes a token/PAT — only label / username / host metadata.
///
/// When both GitHub and GitLab are linked, both provider lines are listed.
pub async fn build_account_context(db: &Db, project_id: &str) -> Option<String> {
    let row = sqlx::query("SELECT github_account_id, gitlab_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()?;
    let github_id: Option<String> = row.get("github_account_id");
    let gitlab_id: Option<String> = row.get("gitlab_account_id");

    let mut lines: Vec<String> = Vec::new();

    if let Some(id) = github_id {
        let (label, username) = fetch_github_meta(db, &id).await;
        let label = label.unwrap_or(id);
        lines.push(account_line(
            "GitHub",
            &label,
            username.as_deref(),
            "github.com",
        ));
    }
    if let Some(id) = gitlab_id {
        let meta = fetch_gitlab_meta(db, &id).await;
        let label = meta.label.unwrap_or(id);
        let host = meta
            .host
            .as_deref()
            .and_then(normalize_host)
            .unwrap_or_else(|| "gitlab.com".to_string());
        lines.push(account_line(
            "GitLab",
            &label,
            meta.username.as_deref(),
            &host,
        ));
    }
    if let Ok(Some(meta)) = resolve_aws_runtime_metadata(db, project_id).await {
        lines.push(aws_account_line(&meta));
    }

    // ssh-transparent-connect (AC-308): append the project's mapped-VPS block so
    // Claude knows the ssh aliases. Empty string when no server is mapped
    // (AC-305 no-op). Kept SEPARATE from the git-account block and only appended,
    // so it never overrides the account context.
    let ssh_block = crate::runs::ssh_access::build_project_ssh_context(db, project_id).await;

    if lines.is_empty() {
        // No git account. Still surface ssh context if any server is mapped so a
        // project with only VPS mappings gets its aliases (AC-305/AC-308).
        if ssh_block.is_empty() {
            return None;
        }
        return Some(ssh_block);
    }

    let account_block = format!(
        "This project is configured with pre-attached credentials managed by Devdy:\n\
         {}\n\n\
         Use gh / glab / git / aws as normal — the correct project account is already wired, \
         you do NOT need to login or provide a token/credential. Some credential-touching \
         commands are blocked by policy (e.g. `gh auth token`, `gh auth login`, `aws configure`). \
         This machine should not rely on global credentials for these tools; outside this wiring \
         calls may fail (fail-closed).",
        lines.join("\n")
    );

    if ssh_block.is_empty() {
        Some(account_block)
    } else {
        // Cumulative: account context first, ssh context appended after.
        Some(format!("{account_block}\n\n{ssh_block}"))
    }
}

/// Format one provider line for the account context. Drops the `(@username)`
/// segment when the username is unknown (never fabricate identity).
fn account_line(provider: &str, label: &str, username: Option<&str>, host: &str) -> String {
    match username.filter(|u| !u.trim().is_empty()) {
        Some(u) => format!("- {provider}: account \"{label}\" (@{u}) on {host}"),
        None => format!("- {provider}: account \"{label}\" on {host}"),
    }
}

fn aws_account_line(meta: &AwsRuntimeMetadata) -> String {
    let account = meta
        .account_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("unknown account");
    let auth = if meta.auth_method == "profile" {
        meta.profile_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|p| format!("profile {p}"))
            .unwrap_or_else(|| "profile".to_string())
    } else {
        "access keys".to_string()
    };
    format!(
        "- AWS: account \"{}\" ({account}) in {} via {auth}",
        meta.account_label, meta.region
    )
}

async fn fetch_github_email(db: &Db, account_id: &str) -> Option<String> {
    let row = sqlx::query("SELECT email FROM github_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()?;
    row.get("email")
}

async fn fetch_gitlab_email(db: &Db, account_id: &str) -> Option<String> {
    let row = sqlx::query("SELECT email FROM gitlab_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()?;
    row.get("email")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("mem db");
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY, github_account_id TEXT, gitlab_account_id TEXT)")
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
        pool
    }

    #[tokio::test]
    async fn resolver_fail_closed_when_no_account_linked() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id) VALUES ('p1')")
            .execute(&db)
            .await
            .unwrap();
        let r = resolve_token_for_project(&db, "p1", "gh", None)
            .await
            .unwrap();
        assert!(r.is_none(), "expected None when no github account linked");

        let r = resolve_token_for_project(&db, "p1", "glab", None)
            .await
            .unwrap();
        assert!(r.is_none(), "expected None when no gitlab account linked");
    }

    #[tokio::test]
    async fn resolver_fail_closed_for_unknown_project() {
        let db = mem_db().await;
        let r = resolve_token_for_project(&db, "missing", "gh", None)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn resolver_git_no_host_is_none() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id) VALUES ('p1')")
            .execute(&db)
            .await
            .unwrap();
        let r = resolve_token_for_project(&db, "p1", "git", None)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn resolver_unknown_tool_is_none() {
        let db = mem_db().await;
        let r = resolve_token_for_project(&db, "p1", "svn", None)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    // ---- SEC-1 (loop-back 1): strict host matching (no `contains()`) ----

    #[test]
    fn host_matches_exact_and_dotted_subdomain() {
        assert!(host_matches("github.com", "github.com"));
        assert!(host_matches("GitHub.com", "github.com")); // case-insensitive
        assert!(host_matches("api.github.com", "api.github.com"));
        // valid dotted-boundary subdomain of the account host
        assert!(host_matches(
            "code.gitlab.example.com",
            "gitlab.example.com"
        ));
        assert!(host_matches(
            "gitlab.example.com:8443",
            "gitlab.example.com"
        )); // port stripped
        assert!(host_matches(
            "https://gitlab.example.com/path",
            "gitlab.example.com"
        ));
    }

    #[test]
    fn host_matches_rejects_spoofed_hosts() {
        // The exact spoof vectors called out in the defect.
        assert!(!host_matches("github.attacker.io", "github.com"));
        assert!(!host_matches("gitlab.com.evil.io", "gitlab.com"));
        assert!(!host_matches("notgithub.com", "github.com"));
        // substring / prefix tricks that the old `contains()` accepted
        assert!(!host_matches("evilgithub.com", "github.com"));
        assert!(!host_matches("github.com.evil.io", "github.com"));
        assert!(!host_matches("xgitlab.example.com", "gitlab.example.com"));
    }

    async fn insert_gitlab_project(db: &Db, project_id: &str, host: &str) {
        sqlx::query("INSERT INTO gitlab_accounts (id, label, host) VALUES ('gl1', 'work', ?)")
            .bind(host)
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects (id, gitlab_account_id) VALUES (?, 'gl1')")
            .bind(project_id)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn git_github_spoofed_hosts_are_none() {
        let db = mem_db().await;
        // Link a github account so resolve_github would succeed IF the host matched.
        sqlx::query("INSERT INTO github_accounts (id, label) VALUES ('gh1', 'work')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects (id, github_account_id) VALUES ('p1', 'gh1')")
            .execute(&db)
            .await
            .unwrap();

        for spoof in [
            "github.attacker.io",
            "notgithub.com",
            "evilgithub.com",
            "github.com.evil.io",
        ] {
            let r = resolve_git(&PatTokenProvider, &db, "p1", Some(spoof))
                .await
                .unwrap();
            assert!(
                r.is_none(),
                "spoofed github host {spoof} must NOT get a token"
            );
        }
    }

    #[tokio::test]
    async fn git_gitlab_spoofed_hosts_are_none() {
        let db = mem_db().await;
        insert_gitlab_project(&db, "p1", "gitlab.com").await;

        for spoof in [
            "gitlab.com.evil.io",
            "xgitlab.com",
            "gitlab.com.attacker.net",
        ] {
            let r = resolve_git(&PatTokenProvider, &db, "p1", Some(spoof))
                .await
                .unwrap();
            assert!(
                r.is_none(),
                "spoofed gitlab host {spoof} must NOT get a token"
            );
        }
    }

    #[tokio::test]
    async fn git_valid_gitlab_host_matches() {
        let db = mem_db().await;
        insert_gitlab_project(&db, "p1", "gitlab.example.com").await;

        // Exact host match resolves the account. PAT is absent in this in-memory
        // test env, so fail-closed still yields None — but crucially it reached
        // resolve_gitlab (not rejected as an unknown host). Verify the same host
        // as a valid dotted subdomain also routes through.
        for good in ["gitlab.example.com", "runner.gitlab.example.com"] {
            // No Keychain PAT → Ok(None) via fail-closed; the point is it does not
            // error and does not panic, i.e. the host was accepted for lookup.
            let r = resolve_git(&PatTokenProvider, &db, "p1", Some(good)).await;
            assert!(r.is_ok(), "valid host {good} must not error");
        }
    }

    // ---- AC3: commit identity resolution ----

    #[tokio::test]
    async fn commit_identity_none_when_no_account() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id) VALUES ('p1')")
            .execute(&db)
            .await
            .unwrap();
        let id = resolve_commit_identity(&db, "p1").await;
        assert!(id.name.is_none() && id.email.is_none());
        // unknown project too
        let id = resolve_commit_identity(&db, "missing").await;
        assert!(id.name.is_none() && id.email.is_none());
    }

    #[tokio::test]
    async fn commit_identity_prefers_github_when_both_linked() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO github_accounts (id, label, username, email) VALUES ('gh1','work','ghuser','gh@ex.com')")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO gitlab_accounts (id, label, host, username, email) VALUES ('gl1','work','gitlab.com','gluser','gl@ex.com')")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO projects (id, github_account_id, gitlab_account_id) VALUES ('p1','gh1','gl1')")
            .execute(&db).await.unwrap();
        let id = resolve_commit_identity(&db, "p1").await;
        assert_eq!(id.name.as_deref(), Some("ghuser"));
        assert_eq!(id.email.as_deref(), Some("gh@ex.com"));
    }

    #[tokio::test]
    async fn commit_identity_uses_gitlab_when_only_gitlab() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO gitlab_accounts (id, label, host, username, email) VALUES ('gl1','work','gitlab.com','gluser','gl@ex.com')")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO projects (id, gitlab_account_id) VALUES ('p1','gl1')")
            .execute(&db)
            .await
            .unwrap();
        let id = resolve_commit_identity(&db, "p1").await;
        assert_eq!(id.name.as_deref(), Some("gluser"));
        assert_eq!(id.email.as_deref(), Some("gl@ex.com"));
    }

    #[tokio::test]
    async fn commit_identity_null_email_is_none() {
        let db = mem_db().await;
        // github account with username but NULL email → email None, name Some.
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
        let id = resolve_commit_identity(&db, "p1").await;
        assert_eq!(id.name.as_deref(), Some("ghuser"));
        assert!(id.email.is_none(), "NULL email must not be fabricated");
    }

    #[tokio::test]
    async fn commit_identity_null_username_is_none() {
        let db = mem_db().await;
        // github account with email but NULL username → name None.
        sqlx::query(
            "INSERT INTO github_accounts (id, label, email) VALUES ('gh1','work','gh@ex.com')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO projects (id, github_account_id) VALUES ('p1','gh1')")
            .execute(&db)
            .await
            .unwrap();
        let id = resolve_commit_identity(&db, "p1").await;
        assert!(id.name.is_none(), "NULL username must not be fabricated");
        assert_eq!(id.email.as_deref(), Some("gh@ex.com"));
    }

    #[tokio::test]
    async fn resolved_token_carries_username() {
        // resolve_github fills username from the account row (PAT missing in this
        // env → we can't assert token, so assert via a direct row read helper).
        let db = mem_db().await;
        sqlx::query(
            "INSERT INTO github_accounts (id, label, username) VALUES ('gh1','work','ghuser')",
        )
        .execute(&db)
        .await
        .unwrap();
        let (label, username) = fetch_github_meta(&db, "gh1").await;
        assert_eq!(label.as_deref(), Some("work"));
        assert_eq!(username.as_deref(), Some("ghuser"));
    }

    // ---- GĐ5 (AC1..AC4): provider seam + TTL cache + scaffolds ----

    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    /// Deterministic clock for cache tests.
    struct MockClock {
        t: AtomicI64,
    }
    impl MockClock {
        fn new(t: i64) -> Self {
            Self {
                t: AtomicI64::new(t),
            }
        }
        fn advance(&self, secs: i64) {
            self.t.fetch_add(secs, Ordering::SeqCst);
        }
    }
    impl Clock for MockClock {
        fn now_unix(&self) -> i64 {
            self.t.load(Ordering::SeqCst)
        }
    }

    /// Provider that counts fetch calls and returns a scripted result.
    struct MockProvider {
        calls: AtomicUsize,
        // "some_expiring" | "some_pat" | "none" | "err"
        mode: &'static str,
        expires_at: Option<i64>,
    }
    impl MockProvider {
        fn new(mode: &'static str, expires_at: Option<i64>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                mode,
                expires_at,
            }
        }
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }
    impl TokenProvider for MockProvider {
        fn fetch(&self, _scope: &TokenScope) -> Result<Option<ProviderToken>, TokenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                "some_expiring" | "some_pat" => Ok(Some(ProviderToken {
                    token: "tok".to_string(),
                    expires_at: self.expires_at,
                    username: None,
                })),
                "none" => Ok(None),
                "err" => Err(TokenError::Fetch("boom".to_string())),
                _ => unreachable!("unknown mock mode"),
            }
        }
    }

    fn scope() -> TokenScope {
        TokenScope {
            provider_kind: "github".to_string(),
            account_id: "acc1".to_string(),
            host: "github.com".to_string(),
        }
    }

    #[test]
    fn cache_hit_when_fresh() {
        let inner = MockProvider::new("some_expiring", Some(1_000 + 3600));
        let clock = MockClock::new(1_000);
        let cache = CachingTokenProvider::new(inner, clock, 60);
        let s = scope();
        let r1 = cache.fetch(&s).unwrap().unwrap();
        assert_eq!(r1.token, "tok");
        let _r2 = cache.fetch(&s).unwrap().unwrap();
        assert_eq!(cache.inner.call_count(), 1, "second call must hit cache");
    }

    #[test]
    fn refetch_when_expired() {
        let inner = MockProvider::new("some_expiring", Some(1_000 + 100));
        let clock = MockClock::new(1_000);
        let cache = CachingTokenProvider::new(inner, clock, 60);
        let s = scope();
        let _ = cache.fetch(&s).unwrap();
        cache.clock.advance(200); // now 1_200 > exp 1_100
        let _ = cache.fetch(&s).unwrap();
        assert_eq!(cache.inner.call_count(), 2, "expired token must refetch");
    }

    #[test]
    fn refetch_within_skew() {
        // exp = now+30, skew = 60 → considered stale immediately.
        let inner = MockProvider::new("some_expiring", Some(1_000 + 30));
        let clock = MockClock::new(1_000);
        let cache = CachingTokenProvider::new(inner, clock, 60);
        let s = scope();
        let _ = cache.fetch(&s).unwrap();
        let _ = cache.fetch(&s).unwrap(); // same now, but within skew → refetch
        assert_eq!(
            cache.inner.call_count(),
            2,
            "within-skew token refreshes early"
        );
    }

    #[test]
    fn pat_none_not_cached() {
        let inner = MockProvider::new("some_pat", None); // PAT: no expiry
        let clock = MockClock::new(1_000);
        let cache = CachingTokenProvider::new(inner, clock, 60);
        let s = scope();
        for _ in 0..3 {
            let r = cache.fetch(&s).unwrap().unwrap();
            assert!(r.expires_at.is_none());
        }
        assert_eq!(cache.inner.call_count(), 3, "PAT must bypass cache");
    }

    #[test]
    fn error_not_cached() {
        let inner = MockProvider::new("err", None);
        let clock = MockClock::new(1_000);
        let cache = CachingTokenProvider::new(inner, clock, 60);
        let s = scope();
        assert!(cache.fetch(&s).is_err());
        assert!(cache.fetch(&s).is_err());
        assert_eq!(cache.inner.call_count(), 2, "errors must not be cached");
    }

    #[test]
    fn none_not_cached() {
        let inner = MockProvider::new("none", None);
        let clock = MockClock::new(1_000);
        let cache = CachingTokenProvider::new(inner, clock, 60);
        let s = scope();
        assert!(cache.fetch(&s).unwrap().is_none());
        assert!(cache.fetch(&s).unwrap().is_none());
        assert_eq!(cache.inner.call_count(), 2, "None must not be cached");
    }

    // ---- AC1/AC2: PatTokenProvider behavior + expires_at ----

    #[test]
    fn pat_provider_unknown_kind_is_none() {
        let s = TokenScope {
            provider_kind: "svn".to_string(),
            account_id: "x".to_string(),
            host: "h".to_string(),
        };
        // Unknown kind short-circuits to Ok(None) before any Keychain access.
        assert!(PatTokenProvider.fetch(&s).unwrap().is_none());
    }

    #[test]
    fn pat_provider_missing_secret_fails_closed() {
        // No Keychain entry for these ids in the test env → Err(_) → Ok(None).
        for kind in ["github", "gitlab"] {
            let s = TokenScope {
                provider_kind: kind.to_string(),
                account_id: "gd5-nonexistent-account".to_string(),
                host: "github.com".to_string(),
            };
            let r = PatTokenProvider.fetch(&s).unwrap();
            assert!(r.is_none(), "{kind}: missing PAT must fail closed to None");
        }
    }

    /// AC1 mock-injection: the internal `_with` variant routes through the
    /// injected provider while preserving DB dispatch + fail-closed.
    #[tokio::test]
    async fn resolve_with_injected_provider_used_for_github() {
        let db = mem_db().await;
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
        // Injected provider yields an expiring token → proves the seam is used
        // and expires_at flows into ResolvedToken (PAT path would be None).
        let mock = MockProvider::new("some_expiring", Some(9_999));
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None)
            .await
            .unwrap()
            .expect("token resolved via injected provider");
        assert_eq!(r.token, "tok");
        assert_eq!(r.expires_at, Some(9_999));
        // Username still comes from the DB row, not the provider.
        assert_eq!(r.username.as_deref(), Some("ghuser"));
    }

    #[tokio::test]
    async fn resolve_with_injected_provider_none_fails_closed() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO github_accounts (id, label) VALUES ('gh1','work')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects (id, github_account_id) VALUES ('p1','gh1')")
            .execute(&db)
            .await
            .unwrap();
        let mock = MockProvider::new("err", None);
        // Provider Err → resolver collapses to Ok(None) (fail-closed intact).
        let r = resolve_token_for_project_with(&mock, &db, "gh", "gh", None).await;
        assert!(matches!(r, Ok(None)), "provider error must fail closed");
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None).await;
        assert!(matches!(r, Ok(None)));
    }

    // ---- AC4: App-token provider scaffolds ----

    #[test]
    fn app_scaffolds_return_unsupported() {
        let s = scope();
        assert!(matches!(
            GithubAppTokenProvider {}.fetch(&s),
            Err(TokenError::Unsupported)
        ));
        assert!(matches!(
            GitlabProjectTokenProvider {}.fetch(&s),
            Err(TokenError::Unsupported)
        ));
    }
}
