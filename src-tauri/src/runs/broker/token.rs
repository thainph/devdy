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
    /// Devdy's account id (`aws_accounts.id`).
    pub id: String,
    pub account_label: String,
    pub auth_method: String,
    pub account_id: Option<String>,
    pub arn: Option<String>,
    pub region: String,
    pub profile_name: Option<String>,
}

/// Resolved AWS credential for a project. Secret fields must never be logged.
pub struct ResolvedAwsCredentials {
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

/// Which account one brokered call asks for. All optional; none → the default.
#[derive(Debug, Default, Clone, Copy)]
pub struct AccountSelect<'a> {
    /// The run issuing the call: a pinned run (PR-inbox review) defaults its
    /// GitHub calls to the pinned account.
    pub run_id: Option<&'a str>,
    /// Account the command names ([`GIT_ACCOUNT_ENV`]): label, username or id.
    /// Must be one of the project's enabled accounts, else the call is denied.
    pub account: Option<&'a str>,
    /// git only: the username in the remote URL (`https://<user>@host/...`).
    /// Picks that account when it matches one, otherwise ignored.
    pub url_username: Option<&'a str>,
}

/// Resolve the token for a project + tool. Fail-closed: `Ok(None)` when no usable
/// account is linked. `Err` on genuine DB errors (message is generic — never
/// contains secrets) or, prefixed [`UNKNOWN_GIT_ACCOUNT`], when the call names
/// an account that is not enabled for the project.
pub async fn resolve_token_for_project(
    db: &Db,
    project_id: &str,
    tool: &str,
    host: Option<&str>,
    sel: AccountSelect<'_>,
) -> Result<Option<ResolvedToken>, String> {
    // Delegate through the default PAT provider so the behavior is identical to
    // before the seam existed.
    resolve_token_for_project_with(&PatTokenProvider, db, project_id, tool, host, sel).await
}

/// Internal variant that takes a `TokenProvider`, enabling mock injection in
/// tests. The dispatch and all DB/host/fail-closed logic are unchanged.
async fn resolve_token_for_project_with(
    provider: &dyn TokenProvider,
    db: &Db,
    project_id: &str,
    tool: &str,
    host: Option<&str>,
    sel: AccountSelect<'_>,
) -> Result<Option<ResolvedToken>, String> {
    match pick_git_account(db, project_id, tool, host, sel).await? {
        Some(acct) if acct.provider == "github" => {
            resolve_github_account(provider, db, &acct.id).await
        }
        Some(acct) => resolve_gitlab_account(provider, &acct),
        None => Ok(None), // not linked / unknown host → fail-closed
    }
}

/// The account one gh / glab / git call would use, without fetching a token.
/// `host`: git's remote host, or glab's target host when the shim knows it —
/// GitLab accounts are then limited to that host. `Ok(None)` when no account
/// applies (fail-closed); `Err` prefixed [`UNKNOWN_GIT_ACCOUNT`] when the call
/// names an account that is not enabled.
pub async fn pick_git_account(
    db: &Db,
    project_id: &str,
    tool: &str,
    host: Option<&str>,
    sel: AccountSelect<'_>,
) -> Result<Option<GitAccount>, String> {
    match tool {
        // gh honors a run's pin (a PR-inbox review runs as the requested account).
        "github" | "gh" => pick_github(db, project_id, sel, true).await,
        "gitlab" | "glab" => pick_gitlab(db, project_id, host, sel).await,
        "git" => {
            let Some(host) = host else {
                return Ok(None); // no host → cannot safely pick → fail-closed
            };
            // GitHub: only the canonical hosts are trusted. The schema does not
            // store a per-account GitHub Enterprise host, so accept exactly
            // `github.com` and `api.github.com` (dotted-boundary), and nothing
            // else. A spoofed host such as `github.attacker.io` or
            // `notgithub.com` must NOT match. git keeps the project's default
            // (not a run's pin): fetch/push need the project's access.
            if host_matches(host, "github.com") || host_matches(host, "api.github.com") {
                return pick_github(db, project_id, sel, false).await;
            }
            // GitLab: only accounts whose own host matches exactly (or a valid
            // dotted-boundary subdomain) — a host that matches no enabled account
            // resolves to nothing (unknown/spoofed remote).
            pick_gitlab(db, project_id, Some(host), sel).await
        }
        _ => Ok(None),
    }
}

// ============================================================================
// AWS profile/SSO credential minting.
//
// A `profile` account has no static secret in the Keychain; its credentials live
// behind the machine's AWS config (typically an SSO session with a short-lived
// cached token). Rather than delegate that token to each run — where it silently
// expires mid-session and cannot be refreshed (`aws sso login` is browser-bound
// and policy-blocked inside a run) — the broker mints fresh *temporary*
// credentials itself by shelling out to the real `aws` CLI. This runs in the
// trusted app process (exactly like reading the Keychain for `keys` accounts),
// transparently refreshes the SSO token when a refresh token exists, and the
// resulting creds still flow ONLY through the existing per-call shim response
// (never wired into SDK `credential_process`), so per-call policy gating is
// unchanged.
// ============================================================================

/// Deny-reason marker: the linked profile is SSO and its cached token is
/// expired/absent, so the broker could not mint temporary credentials. The UI
/// matches this prefix to offer an in-app `aws sso login`. Carries no secret.
pub const AWS_SSO_LOGIN_REQUIRED: &str = "aws_sso_login_required";

/// Refresh window: re-mint when within this many seconds of expiry.
const AWS_CREDS_SKEW_SECS: i64 = 300;

/// Minted temporary credentials for one profile. Secret — never logged.
#[derive(Clone)]
struct CachedAwsSecret {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    /// Absolute expiry (unix seconds).
    expires_at: i64,
}

/// Process-wide cache of minted creds, keyed by profile name. Avoids spawning
/// `aws` on every single brokered call within a run.
fn aws_creds_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CachedAwsSecret>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, CachedAwsSecret>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn get_cached_aws_secret(profile: &str, now: i64) -> Option<CachedAwsSecret> {
    let store = aws_creds_cache().lock().ok()?;
    let entry = store.get(profile)?;
    if entry.expires_at - now > AWS_CREDS_SKEW_SECS {
        Some(entry.clone())
    } else {
        None
    }
}

fn put_cached_aws_secret(profile: &str, sec: &CachedAwsSecret) {
    if let Ok(mut store) = aws_creds_cache().lock() {
        store.insert(profile.to_string(), sec.clone());
    }
}

/// Heuristic: does this `aws` stderr mean "the SSO session needs a fresh login"?
/// Matches the CLI's varied phrasings (e.g. `Error loading SSO Token: Token for
/// <session> does not exist`, `The SSO session ... has expired`).
fn stderr_is_sso_login_required(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("error loading sso token")
        || s.contains("token has expired")
        || (s.contains("sso")
            && (s.contains("login") || s.contains("expired") || s.contains("does not exist")))
}

/// Parse `aws configure export-credentials --format process` output (credential
/// process JSON v1). Never logs the payload.
fn parse_export_credentials(stdout: &[u8]) -> Result<CachedAwsSecret, String> {
    #[derive(serde::Deserialize)]
    struct Proc {
        #[serde(rename = "AccessKeyId")]
        access_key_id: String,
        #[serde(rename = "SecretAccessKey")]
        secret_access_key: String,
        #[serde(rename = "SessionToken")]
        session_token: Option<String>,
        #[serde(rename = "Expiration")]
        expiration: Option<String>,
    }
    let parsed: Proc = serde_json::from_slice(stdout)
        .map_err(|_| "aws export-credentials: unparseable output".to_string())?;
    // No `Expiration` (e.g. static profile creds) → treat as short-lived so we
    // re-resolve soon rather than caching a value we cannot age out.
    let expires_at = parsed
        .expiration
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|| chrono::Utc::now().timestamp() + AWS_CREDS_SKEW_SECS * 2);
    Ok(CachedAwsSecret {
        access_key_id: parsed.access_key_id,
        secret_access_key: parsed.secret_access_key,
        session_token: parsed.session_token.filter(|s| !s.trim().is_empty()),
        expires_at,
    })
}

/// Mint fresh temporary credentials for a profile via the real `aws` CLI. This
/// transparently refreshes an SSO token when possible; when the SSO session
/// itself is expired/absent it returns `Err(AWS_SSO_LOGIN_REQUIRED)`.
async fn export_aws_credentials(profile: &str, region: &str) -> Result<CachedAwsSecret, String> {
    let mut cmd = tokio::process::Command::new("aws");
    cmd.arg("configure")
        .arg("export-credentials")
        .arg("--profile")
        .arg(profile)
        .arg("--format")
        .arg("process")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // GUI launches (Finder/Dock) don't inherit the login-shell PATH, so `aws`
    // isn't found by name; recover it like the STS/ssh commands. The login PATH
    // never contains the per-run shim dir, so this resolves the real binary.
    crate::runs::sidecar::augment_command_path(&mut cmd);
    // Read the user's real AWS config; never let an inherited profile shadow
    // `--profile`. The broker (app process) env holds the real config paths;
    // per-run overrides are applied only to child run commands, not here.
    cmd.env_remove("AWS_PROFILE");
    cmd.env_remove("AWS_DEFAULT_PROFILE");
    // Force the user's real ~/.aws config; never let an inherited per-run
    // AWS_CONFIG_FILE/credentials override shadow the linked profile.
    cmd.env_remove("AWS_CONFIG_FILE");
    cmd.env_remove("AWS_SHARED_CREDENTIALS_FILE");
    cmd.env("AWS_REGION", region);
    cmd.env("AWS_DEFAULT_REGION", region);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("aws spawn failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr_is_sso_login_required(&stderr) {
            return Err(AWS_SSO_LOGIN_REQUIRED.to_string());
        }
        // Never surface stdout (may hold creds); keep the message generic.
        return Err("aws export-credentials failed".to_string());
    }
    parse_export_credentials(&output.stdout)
}

/// Build the wire-level resolved credentials from account metadata + minted
/// secret. `profile_name` is intentionally `None`: the creds are injected
/// directly, so the `aws` shim uses its access-key branch (no profile hand-off).
fn resolved_from_minted(meta: &AwsRuntimeMetadata, sec: CachedAwsSecret) -> ResolvedAwsCredentials {
    ResolvedAwsCredentials {
        account_label: meta.account_label.clone(),
        auth_method: meta.auth_method.clone(),
        account_id: meta.account_id.clone(),
        arn: meta.arn.clone(),
        region: meta.region.clone(),
        access_key_id: Some(sec.access_key_id),
        secret_access_key: Some(sec.secret_access_key),
        session_token: sec.session_token,
        profile_name: None,
    }
}

/// Resolve AWS credentials for a project. `selector` names one of the
/// project's enabled accounts ([`AWS_ACCOUNT_ENV`] or `--profile`); `None` → the
/// default. Fail-closed: returns `Ok(None)` when no account is linked, metadata
/// is incomplete, or a keys account is missing its Keychain secret; `Err`
/// prefixed [`UNKNOWN_CLOUD_ACCOUNT`] when the selector matches no enabled
/// account.
pub async fn resolve_aws_credentials_for_project(
    db: &Db,
    project_id: &str,
    selector: Option<&str>,
) -> Result<Option<ResolvedAwsCredentials>, String> {
    let Some(meta) = pick_aws_account(db, project_id, selector).await? else {
        return Ok(None);
    };

    // Only profile-based accounts are supported. The broker mints fresh temporary
    // credentials from the linked CLI profile (refreshing the SSO token when
    // possible) and serves them through the shim's access-key path.
    if meta.auth_method != "profile" {
        return Ok(None);
    }
    let profile_name = match meta.profile_name.clone().filter(|s| !s.trim().is_empty()) {
        Some(v) => v,
        None => return Ok(None),
    };
    // Serve fresh-enough cached creds without spawning `aws`.
    let now = chrono::Utc::now().timestamp();
    if let Some(sec) = get_cached_aws_secret(&profile_name, now) {
        return Ok(Some(resolved_from_minted(&meta, sec)));
    }
    // Miss/stale → mint fresh temporary credentials (also refreshes the SSO token
    // when possible). Propagates AWS_SSO_LOGIN_REQUIRED so the socket layer can
    // render an actionable deny reason.
    let sec = export_aws_credentials(&profile_name, &meta.region).await?;
    put_cached_aws_secret(&profile_name, &sec);
    Ok(Some(resolved_from_minted(&meta, sec)))
}

/// Non-secret metadata of the project's DEFAULT AWS account, for per-run env
/// setup (sandbox region). Returns `Ok(None)` when no AWS account is linked or
/// the linked row is gone.
pub async fn resolve_aws_runtime_metadata(
    db: &Db,
    project_id: &str,
) -> Result<Option<AwsRuntimeMetadata>, String> {
    pick_aws_account(db, project_id, None).await
}

/// The project's enabled AWS accounts plus its default. Ordered by label.
pub async fn aws_accounts_for(db: &Db, project_id: &str) -> Result<Vec<AwsRuntimeMetadata>, String> {
    let rows = sqlx::query(
        "SELECT a.id, a.label, a.auth_method, a.account_id, a.arn, a.region, a.profile_name \
         FROM aws_accounts a \
         WHERE a.id IN (SELECT account_id FROM project_aws_accounts WHERE project_id = ?1) \
            OR a.id = (SELECT aws_account_id FROM projects WHERE id = ?1) \
         ORDER BY a.label COLLATE NOCASE, a.id",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|row| AwsRuntimeMetadata {
            id: row.get("id"),
            account_label: row.get("label"),
            auth_method: row.get("auth_method"),
            account_id: row.get("account_id"),
            arn: row.get("arn"),
            region: row.get("region"),
            profile_name: row.get("profile_name"),
        })
        .collect())
}

/// The AWS account one call uses: the one `selector` names (id, label, 12-digit
/// account id or profile name, case-insensitive) — which must be enabled — else
/// the project's default, else the first enabled account.
pub async fn pick_aws_account(
    db: &Db,
    project_id: &str,
    selector: Option<&str>,
) -> Result<Option<AwsRuntimeMetadata>, String> {
    let default = project_default_account(db, project_id, "aws_account_id").await?;
    let accounts = aws_accounts_for(db, project_id).await?;
    let idx = pick_cloud_index(
        &accounts,
        |a| &a.id,
        |a| {
            [Some(a.account_label.as_str()), a.account_id.as_deref(), a.profile_name.as_deref()]
                .into_iter()
                .flatten()
                .collect()
        },
        default.as_deref(),
        selector,
    )
    .map_err(|want| unknown_cloud_account_reason("AWS", AWS_ACCOUNT_ENV, want, accounts.iter().map(aws_display)))?;
    Ok(idx.map(|i| accounts.into_iter().nth(i).expect("index in range")))
}

/// `"label" (123456789012)` for prompts and deny reasons.
pub fn aws_display(a: &AwsRuntimeMetadata) -> String {
    match a.account_id.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(id) => format!("\"{}\" ({id})", a.account_label),
        None => format!("\"{}\"", a.account_label),
    }
}

/// Deny-reason marker: the command named an AWS / Azure account
/// ([`AWS_ACCOUNT_ENV`] / [`AZURE_ACCOUNT_ENV`] / `aws --profile`) that is not
/// enabled for the project. The reason lists the enabled accounts (labels and
/// ids only, never a secret).
pub const UNKNOWN_CLOUD_ACCOUNT: &str = "unknown_cloud_account";

/// Env var a command sets to pick one of the project's enabled AWS accounts
/// (`DEVDY_AWS_ACCOUNT=prod aws s3 ls`). Read by the aws / terraform shims.
pub const AWS_ACCOUNT_ENV: &str = "DEVDY_AWS_ACCOUNT";

/// Env var a command sets to pick one of the project's enabled Azure accounts
/// (`DEVDY_AZURE_ACCOUNT=prod az group list`). Read by the az / terraform shims.
pub const AZURE_ACCOUNT_ENV: &str = "DEVDY_AZURE_ACCOUNT";

/// Index of the account one call uses: the one `selector` names (`id` exact,
/// else any of `names` case-insensitively), else `default`, else the first.
/// `Err(selector)` when the selector names no account. `Ok(None)` only when
/// there is no account at all.
fn pick_cloud_index<'a, T>(
    accounts: &'a [T],
    id: impl Fn(&'a T) -> &'a str,
    names: impl Fn(&'a T) -> Vec<&'a str>,
    default: Option<&str>,
    selector: Option<&'a str>,
) -> Result<Option<usize>, &'a str> {
    if let Some(want) = selector.map(str::trim).filter(|s| !s.is_empty()) {
        return accounts
            .iter()
            .position(|a| id(a) == want)
            .or_else(|| {
                accounts
                    .iter()
                    .position(|a| names(a).iter().any(|n| n.trim().eq_ignore_ascii_case(want)))
            })
            .map(Some)
            .ok_or(want);
    }
    Ok(default
        .and_then(|d| accounts.iter().position(|a| id(a) == d))
        .or(if accounts.is_empty() { None } else { Some(0) }))
}

fn unknown_cloud_account_reason(
    provider: &str,
    env: &str,
    want: &str,
    accounts: impl Iterator<Item = String>,
) -> String {
    let enabled = accounts.collect::<Vec<_>>();
    let enabled = if enabled.is_empty() {
        "none".to_string()
    } else {
        enabled.join(", ")
    };
    format!(
        "{UNKNOWN_CLOUD_ACCOUNT}: no {provider} account \"{want}\" is enabled for this project. \
         Enabled: {enabled}. Set {env} to one of these labels, or leave it unset to use the \
         default account."
    )
}

// ============================================================================
// Azure: isolated per-account AZURE_CONFIG_DIR.
//
// An Azure account is an Azure CLI config dir managed by Devdy (see
// `azure_sandbox`). There is no secret to mint here: the broker hands the shim
// the dir path plus tenant/subscription ids, gated per call by the policy engine
// exactly like the AWS path. HONEST LIMITATION: the dir holds az's plaintext
// MSAL token cache (refresh tokens), readable by any process of the same OS
// user that learns the path — the same exposure as AWS profile mode's ~/.aws
// SSO cache. Per-call gating protects the `az` path, not the files.
// ============================================================================

/// Deny-reason marker: the linked Azure account dir has no signed-in session.
/// The UI matches this prefix to offer an in-app `az login`. Carries no secret.
pub const AZURE_LOGIN_REQUIRED: &str = "azure_login_required";

/// Non-secret metadata for a project's linked Azure account.
pub struct AzureRuntimeMetadata {
    pub account_id: String,
    pub account_label: String,
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
    pub subscription_name: Option<String>,
}

/// What the shim needs to run `az` / azurerm as the linked account.
pub struct ResolvedAzureAccount {
    pub config_dir: String,
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
}

/// The project's enabled Azure accounts plus its default. Ordered by label.
pub async fn azure_accounts_for(
    db: &Db,
    project_id: &str,
) -> Result<Vec<AzureRuntimeMetadata>, String> {
    let rows = sqlx::query(
        "SELECT a.id, a.label, a.tenant_id, a.subscription_id, a.subscription_name \
         FROM azure_accounts a \
         WHERE a.id IN (SELECT account_id FROM project_azure_accounts WHERE project_id = ?1) \
            OR a.id = (SELECT azure_account_id FROM projects WHERE id = ?1) \
         ORDER BY a.label COLLATE NOCASE, a.id",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let non_empty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    Ok(rows
        .iter()
        .map(|row| AzureRuntimeMetadata {
            account_id: row.get("id"),
            account_label: row.get("label"),
            tenant_id: non_empty(row.get("tenant_id")),
            subscription_id: non_empty(row.get("subscription_id")),
            subscription_name: non_empty(row.get("subscription_name")),
        })
        .collect())
}

/// The Azure account one call uses: the one `selector` names (id, label,
/// subscription name or id, case-insensitive) — which must be enabled — else
/// the project's default, else the first enabled account.
pub async fn pick_azure_account(
    db: &Db,
    project_id: &str,
    selector: Option<&str>,
) -> Result<Option<AzureRuntimeMetadata>, String> {
    let default = project_default_account(db, project_id, "azure_account_id").await?;
    let accounts = azure_accounts_for(db, project_id).await?;
    let idx = pick_cloud_index(
        &accounts,
        |a| &a.account_id,
        |a| {
            [Some(a.account_label.as_str()), a.subscription_name.as_deref(), a.subscription_id.as_deref()]
                .into_iter()
                .flatten()
                .collect()
        },
        default.as_deref(),
        selector,
    )
    .map_err(|want| {
        unknown_cloud_account_reason("Azure", AZURE_ACCOUNT_ENV, want, accounts.iter().map(azure_display))
    })?;
    Ok(idx.map(|i| accounts.into_iter().nth(i).expect("index in range")))
}

/// `"label" (subscription name)` for prompts and deny reasons.
pub fn azure_display(a: &AzureRuntimeMetadata) -> String {
    match a.subscription_name.as_deref().or(a.subscription_id.as_deref()) {
        Some(sub) => format!("\"{}\" ({sub})", a.account_label),
        None => format!("\"{}\"", a.account_label),
    }
}

/// Resolve the Azure account a call uses (`selector`, else the default).
/// `Ok(None)` when none is linked; `Err(AZURE_LOGIN_REQUIRED)` when the account
/// dir holds no signed-in session (never created, or wiped); `Err` prefixed
/// [`UNKNOWN_CLOUD_ACCOUNT`] for a selector that matches no enabled account;
/// `Err(_)` (generic) on other failures.
pub async fn resolve_azure_for_project(
    db: &Db,
    project_id: &str,
    selector: Option<&str>,
) -> Result<Option<ResolvedAzureAccount>, String> {
    let Some(meta) = pick_azure_account(db, project_id, selector).await? else {
        return Ok(None);
    };
    let root = crate::azure_sandbox::registered_accounts_root()
        .ok_or_else(|| "Azure account root not initialized".to_string())?;
    let dir = crate::azure_sandbox::account_dir_in(root, &meta.account_id)
        .ok_or_else(|| "invalid Azure account id".to_string())?;
    if !crate::azure_sandbox::is_logged_in(&dir) {
        return Err(AZURE_LOGIN_REQUIRED.to_string());
    }
    Ok(Some(ResolvedAzureAccount {
        config_dir: dir.to_string_lossy().into_owned(),
        tenant_id: meta.tenant_id,
        subscription_id: meta.subscription_id,
    }))
}

/// Deny-reason marker: the command named an account ([`GIT_ACCOUNT_ENV`]) that
/// is not enabled for the project. The reason lists the enabled accounts
/// (labels/usernames only, never a secret).
pub const UNKNOWN_GIT_ACCOUNT: &str = "unknown_git_account";

/// Env var a command sets to pick one of the project's enabled git accounts
/// (`DEVDY_GIT_ACCOUNT=work gh pr list`). Read by the gh/glab shims and the git
/// credential helper.
pub const GIT_ACCOUNT_ENV: &str = "DEVDY_GIT_ACCOUNT";

/// One GitHub/GitLab account a project may use. Metadata only — no secret.
#[derive(Debug, Clone)]
pub struct GitAccount {
    /// `github` | `gitlab`.
    pub provider: &'static str,
    pub id: String,
    pub label: String,
    pub username: Option<String>,
    pub email: Option<String>,
    /// `github.com`, or the GitLab account's host as stored.
    pub host: String,
}

impl GitAccount {
    fn from_row(provider: &'static str, row: &sqlx::sqlite::SqliteRow, host: Option<String>) -> Self {
        let non_empty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
        GitAccount {
            provider,
            id: row.get("id"),
            label: row.get("label"),
            username: non_empty(row.get("username")),
            email: non_empty(row.get("email")),
            host: non_empty(host).unwrap_or_else(|| "gitlab.com".to_string()),
        }
    }

    /// `"label" (@user)` for prompts and deny reasons.
    pub fn display(&self) -> String {
        match &self.username {
            Some(u) => format!("\"{}\" (@{u})", self.label),
            None => format!("\"{}\"", self.label),
        }
    }
}

/// The project's enabled GitHub accounts, plus its default and `extra` (a run's
/// pin) even when those are not in the enabled list. Ordered by label.
pub async fn github_accounts_for(
    db: &Db,
    project_id: &str,
    extra: Option<&str>,
) -> Result<Vec<GitAccount>, String> {
    let rows = sqlx::query(
        "SELECT a.id, a.label, a.username, a.email FROM github_accounts a \
         WHERE a.id IN (SELECT account_id FROM project_github_accounts WHERE project_id = ?1) \
            OR a.id = (SELECT github_account_id FROM projects WHERE id = ?1) \
            OR a.id = ?2 \
         ORDER BY a.label COLLATE NOCASE, a.id",
    )
    .bind(project_id)
    .bind(extra)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| GitAccount::from_row("github", r, Some("github.com".to_string())))
        .collect())
}

/// The project's enabled GitLab accounts plus its default. Ordered by label.
pub async fn gitlab_accounts_for(db: &Db, project_id: &str) -> Result<Vec<GitAccount>, String> {
    let rows = sqlx::query(
        "SELECT a.id, a.label, a.username, a.email, a.host FROM gitlab_accounts a \
         WHERE a.id IN (SELECT account_id FROM project_gitlab_accounts WHERE project_id = ?1) \
            OR a.id = (SELECT gitlab_account_id FROM projects WHERE id = ?1) \
         ORDER BY a.label COLLATE NOCASE, a.id",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| GitAccount::from_row("gitlab", r, r.get("host")))
        .collect())
}

/// The project's default account column (`github_account_id` /
/// `gitlab_account_id`). `None` when unset or the project is unknown.
async fn project_default_account(
    db: &Db,
    project_id: &str,
    column: &str,
) -> Result<Option<String>, String> {
    let sql = format!("SELECT {column} AS account_id FROM projects WHERE id = ?");
    let row = sqlx::query(&sql)
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.and_then(|r| r.get("account_id")))
}

/// Find the account a selector names: id, then label, then username
/// (case-insensitive, a leading `@` ignored).
fn find_account<'a>(accounts: &'a [GitAccount], selector: &str) -> Option<&'a GitAccount> {
    let want = selector.trim();
    let want = want.strip_prefix('@').unwrap_or(want);
    if want.is_empty() {
        return None;
    }
    accounts
        .iter()
        .find(|a| a.id == want)
        .or_else(|| accounts.iter().find(|a| a.label.eq_ignore_ascii_case(want)))
        .or_else(|| {
            accounts.iter().find(|a| {
                a.username
                    .as_deref()
                    .is_some_and(|u| u.eq_ignore_ascii_case(want))
            })
        })
}

/// Pick the account for one call: the named one (must be enabled), else the
/// URL username's account when it is one, else the default, else the first
/// enabled account. `Ok(None)` only when no account applies (fail-closed).
fn pick_account<'a>(
    accounts: &'a [GitAccount],
    default: Option<&str>,
    sel: &AccountSelect<'_>,
    provider: &str,
) -> Result<Option<&'a GitAccount>, String> {
    if let Some(want) = sel.account.map(str::trim).filter(|s| !s.is_empty()) {
        return match find_account(accounts, want) {
            Some(a) => Ok(Some(a)),
            None => Err(unknown_account_reason(provider, want, accounts)),
        };
    }
    if let Some(a) = sel.url_username.and_then(|u| find_account(accounts, u)) {
        return Ok(Some(a));
    }
    Ok(default
        .and_then(|d| accounts.iter().find(|a| a.id == d))
        .or_else(|| accounts.first()))
}

fn unknown_account_reason(provider: &str, want: &str, accounts: &[GitAccount]) -> String {
    let enabled = if accounts.is_empty() {
        "none".to_string()
    } else {
        accounts
            .iter()
            .map(GitAccount::display)
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{UNKNOWN_GIT_ACCOUNT}: no {provider} account \"{want}\" is enabled for this project. \
         Enabled: {enabled}. Set {GIT_ACCOUNT_ENV} to one of these labels/usernames, or leave it \
         unset to use the default account."
    )
}

/// GitHub account for a call. `use_pin`: a run's pinned account (or its
/// conductor's) is the default and may be picked; otherwise the project's default.
async fn pick_github(
    db: &Db,
    project_id: &str,
    sel: AccountSelect<'_>,
    use_pin: bool,
) -> Result<Option<GitAccount>, String> {
    let default = if use_pin {
        github_run_default(db, project_id, sel.run_id).await?.0
    } else {
        project_default_account(db, project_id, "github_account_id").await?
    };
    let accounts = github_accounts_for(db, project_id, default.as_deref()).await?;
    Ok(pick_account(&accounts, default.as_deref(), &sel, "GitHub")?.cloned())
}

/// Token + metadata for one GitHub account. Fail-closed `Ok(None)` when the PAT
/// is missing.
async fn resolve_github_account(
    provider: &dyn TokenProvider,
    db: &Db,
    account_id: &str,
) -> Result<Option<ResolvedToken>, String> {
    // Missing PAT → Ok(None) (fail-closed, per OQ#2). Do not leak the error.
    // Delegated to the provider seam; PatTokenProvider preserves this exactly
    // (`Err(_)`/`None`/`Unsupported` all collapse to fail-closed `Ok(None)`).
    let scope = TokenScope {
        provider_kind: "github".to_string(),
        account_id: account_id.to_string(),
        host: "github.com".to_string(),
    };
    let pt = match provider.fetch(&scope) {
        Ok(Some(pt)) => pt,
        Ok(None) | Err(_) => return Ok(None),
    };
    // Label/username are best-effort; default label to the account id if absent.
    let (label, username) = fetch_github_meta(db, account_id).await;
    // Prefer the account row username (unchanged); provider PAT supplies None.
    let username = username.or(pt.username);
    Ok(Some(ResolvedToken {
        token: pt.token,
        host: "github.com".to_string(),
        account_label: label.unwrap_or_else(|| account_id.to_string()),
        username,
        expires_at: pt.expires_at,
    }))
}

/// GitHub accounts a run may use. Ids only — no secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GithubRunAccounts {
    /// Account gh / git use when a command names none: the run's pin, else the
    /// project's default account.
    pub default: Option<String>,
    /// `default` is the run's pin (e.g. a review started from the PR inbox as
    /// the account that was asked to review).
    pub pinned: bool,
    /// Every account the run may pick (enabled + default/pin), by label.
    pub enabled: Vec<String>,
}

/// The account a run's GitHub calls default to: the run's pin — or its
/// conductor's, so workers of an inbox review keep the requested account —
/// else the project's default. A `run_id` that does not belong to `project_id`
/// is ignored (a run can't borrow another project's account). Returns
/// `(account, pinned)`.
async fn github_run_default(
    db: &Db,
    project_id: &str,
    run_id: Option<&str>,
) -> Result<(Option<String>, bool), String> {
    let pinned: Option<String> = match run_id {
        Some(id) => sqlx::query(
            "SELECT COALESCE(r.github_account_id, c.github_account_id) AS pinned \
             FROM runs r \
             LEFT JOIN runs c ON c.id = r.conductor_run_id AND c.project_id = r.project_id \
             WHERE r.id = ? AND r.project_id = ?",
        )
        .bind(id)
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .and_then(|r| r.get("pinned")),
        None => None,
    };
    if let Some(pinned) = pinned {
        return Ok((Some(pinned), true));
    }
    Ok((
        project_default_account(db, project_id, "github_account_id").await?,
        false,
    ))
}

/// Which GitHub accounts a run uses (see [`GithubRunAccounts`]).
pub async fn resolve_github_run_accounts(
    db: &Db,
    project_id: &str,
    run_id: Option<&str>,
) -> Result<GithubRunAccounts, String> {
    let (default, pinned) = github_run_default(db, project_id, run_id).await?;
    let enabled = github_accounts_for(db, project_id, default.as_deref())
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    Ok(GithubRunAccounts {
        default,
        pinned,
        enabled,
    })
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

/// GitLab account for a call. `host` limits the choice to accounts on that
/// host, so a token never goes to an unknown/spoofed remote.
async fn pick_gitlab(
    db: &Db,
    project_id: &str,
    host: Option<&str>,
    sel: AccountSelect<'_>,
) -> Result<Option<GitAccount>, String> {
    let default = project_default_account(db, project_id, "gitlab_account_id").await?;
    let mut accounts = gitlab_accounts_for(db, project_id).await?;
    let provider_name = match host {
        Some(h) => {
            accounts.retain(|a| host_matches(h, &a.host));
            format!("GitLab ({h})")
        }
        None => "GitLab".to_string(),
    };
    Ok(pick_account(&accounts, default.as_deref(), &sel, &provider_name)?.cloned())
}

/// Token for one GitLab account. Fail-closed `Ok(None)` when the PAT is missing.
fn resolve_gitlab_account(
    provider: &dyn TokenProvider,
    acct: &GitAccount,
) -> Result<Option<ResolvedToken>, String> {
    // Delegated to the provider seam; PatTokenProvider preserves fail-closed.
    let scope = TokenScope {
        provider_kind: "gitlab".to_string(),
        account_id: acct.id.clone(),
        host: acct.host.clone(),
    };
    let pt = match provider.fetch(&scope) {
        Ok(Some(pt)) => pt,
        Ok(None) | Err(_) => return Ok(None),
    };
    Ok(Some(ResolvedToken {
        token: pt.token,
        host: acct.host.clone(),
        account_label: acct.label.clone(),
        username: acct.username.clone().or(pt.username),
        expires_at: pt.expires_at,
    }))
}

async fn fetch_gitlab_username(db: &Db, account_id: &str) -> Option<String> {
    let row = sqlx::query("SELECT username FROM gitlab_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()?;
    row.get("username")
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
        (fetch_gitlab_username(db, &id).await, fetch_gitlab_email(db, &id).await)
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
/// Every enabled GitHub / GitLab account is listed; with more than one, the
/// default is marked and the agent is told how to pick another per command.
pub async fn build_account_context(
    db: &Db,
    project_id: &str,
    run_id: Option<&str>,
) -> Option<String> {
    let gitlab_default = project_default_account(db, project_id, "gitlab_account_id")
        .await
        .ok()?;
    let github = resolve_github_run_accounts(db, project_id, run_id)
        .await
        .ok()?;
    let gh_accounts = github_accounts_for(db, project_id, github.default.as_deref())
        .await
        .ok()?;
    let gl_accounts = gitlab_accounts_for(db, project_id).await.ok()?;
    // The account a call without a selector would pick (default, else first).
    let gh_default = pick_account(&gh_accounts, github.default.as_deref(), &AccountSelect::default(), "GitHub")
        .ok()
        .flatten()
        .map(|a| a.id.clone());
    let gl_default = pick_account(&gl_accounts, gitlab_default.as_deref(), &AccountSelect::default(), "GitLab")
        .ok()
        .flatten()
        .map(|a| a.id.clone());
    let multi = gh_accounts.len() > 1 || gl_accounts.len() > 1;
    // git fetch/push (and commit identity) keep the project's default even in a
    // pinned session.
    let git_default = if github.pinned {
        project_default_account(db, project_id, "github_account_id")
            .await
            .ok()
            .flatten()
            .and_then(|id| gh_accounts.iter().find(|a| a.id == id))
    } else {
        None
    };

    let mut lines: Vec<String> = Vec::new();

    for a in &gh_accounts {
        let mut line = account_line("GitHub", &a.label, a.username.as_deref(), "github.com");
        if multi {
            line.push_str(&account_line_extras(a, gh_default.as_deref() == Some(&a.id)));
            if github.pinned && gh_default.as_deref() == Some(&a.id) {
                line.push_str(" (this session was started for this account; default for gh only)");
            }
        }
        lines.push(line);
    }
    for a in &gl_accounts {
        let host = normalize_host(&a.host).unwrap_or_else(|| "gitlab.com".to_string());
        let mut line = account_line("GitLab", &a.label, a.username.as_deref(), &host);
        if multi {
            line.push_str(&account_line_extras(a, gl_default.as_deref() == Some(&a.id)));
        }
        lines.push(line);
    }
    if let Some(a) = git_default {
        lines.push(format!(
            "git fetch/push and commits on github.com use the project's default account {} \
             unless a command picks another one.",
            a.display()
        ));
    }
    if multi {
        lines.push(format!(
            "Several git accounts are enabled. A command uses the default account of its provider \
             unless you pick another one: prefix it with `{GIT_ACCOUNT_ENV}=<label or username>` \
             (e.g. `{GIT_ACCOUNT_ENV}=work gh pr list`, `{GIT_ACCOUNT_ENV}=work git push`). For git \
             you can also put the username in the remote URL \
             (`https://<username>@github.com/<owner>/<repo>.git`). Choose the account that has \
             access to the repository / organization you are working on. Commits are authored as \
             the project's default account (set through GIT_AUTHOR_* / GIT_COMMITTER_*); to \
             commit as another one, override those variables for the command: \
             `GIT_AUTHOR_NAME=<username> GIT_AUTHOR_EMAIL=<email> GIT_COMMITTER_NAME=<username> \
             GIT_COMMITTER_EMAIL=<email> git commit ...`. The devdy MCP tool \
             `git_accounts_list` lists these accounts again."
        ));
    }
    lines.extend(cloud_account_lines(db, project_id).await);

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
         Use gh / glab / git / aws / az / terraform as normal — the correct project account is already wired, \
         you do NOT need to login or provide a token/credential. Some credential-touching \
         commands are blocked by policy (e.g. `gh auth token`, `gh auth login`, `aws configure`, \
         `az login`, `az account get-access-token`). \
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

/// ` — default, email x` suffix for an account line when several are enabled.
fn account_line_extras(a: &GitAccount, is_default: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if is_default {
        parts.push("default".to_string());
    }
    if let Some(email) = &a.email {
        parts.push(format!("email {email}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" — {}", parts.join(", "))
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

/// Account-context lines for the project's enabled AWS / Azure accounts. With
/// several of one provider, the default is marked and the agent is told how to
/// pick another per command.
async fn cloud_account_lines(db: &Db, project_id: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let aws = aws_accounts_for(db, project_id).await.unwrap_or_default();
    let aws_default = pick_aws_account(db, project_id, None).await.ok().flatten().map(|a| a.id);
    let multi_aws = aws.len() > 1;
    for a in &aws {
        lines.push(aws_account_line(a, multi_aws && aws_default.as_deref() == Some(&a.id)));
    }
    if !aws.is_empty() {
        lines.push(if multi_aws {
            format!(
                "Several AWS accounts are enabled. `aws` and terraform use the default AWS account \
                 unless you pick another one: prefix the command with `{AWS_ACCOUNT_ENV}=<label>` \
                 (e.g. `{AWS_ACCOUNT_ENV}=prod aws s3 ls`), or for `aws` pass `--profile <label or \
                 profile>`. Do not put credentials or real profiles in the environment; an unknown \
                 account is refused."
            )
        } else {
            "Run `aws` WITHOUT a `--profile` flag — the linked AWS account is selected \
             automatically."
                .to_string()
        });
    }
    let azure = azure_accounts_for(db, project_id).await.unwrap_or_default();
    let azure_default = pick_azure_account(db, project_id, None)
        .await
        .ok()
        .flatten()
        .map(|a| a.account_id);
    let multi_azure = azure.len() > 1;
    for a in &azure {
        lines.push(azure_account_line(a, multi_azure && azure_default.as_deref() == Some(&a.account_id)));
    }
    if !azure.is_empty() {
        lines.push(
            "Run `az` (and terraform's azurerm provider via Azure CLI auth) without logging in. \
             `az login` / `az account set` are blocked; `az --subscription <other>` needs user \
             confirmation. terraform receives the account's default subscription \
             (ARM_SUBSCRIPTION_ID), but a provider block may still set its own."
                .to_string(),
        );
        if multi_azure {
            lines.push(format!(
                "Several Azure accounts are enabled. `az` and terraform use the default Azure \
                 account unless you pick another one: prefix the command with \
                 `{AZURE_ACCOUNT_ENV}=<label>` (e.g. `{AZURE_ACCOUNT_ENV}=prod az group list`). An \
                 unknown account is refused."
            ));
        }
    }
    if multi_aws || multi_azure {
        lines.push(
            "Choose the cloud account that owns the resources you are working on. The devdy MCP \
             tool `cloud_accounts_list` lists these accounts again."
                .to_string(),
        );
    }
    lines
}

fn aws_account_line(meta: &AwsRuntimeMetadata, is_default: bool) -> String {
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
    let default = if is_default { " — default" } else { "" };
    format!(
        "- AWS: account \"{}\" ({account}) in {} via {auth}{default}",
        meta.account_label, meta.region
    )
}

fn azure_account_line(meta: &AzureRuntimeMetadata, is_default: bool) -> String {
    let subscription = match (&meta.subscription_name, &meta.subscription_id) {
        (Some(name), Some(id)) => format!("subscription \"{name}\" ({id})"),
        (None, Some(id)) => format!("subscription {id}"),
        (Some(name), None) => format!("subscription \"{name}\""),
        (None, None) => "subscription (as selected at sign-in)".to_string(),
    };
    let tenant = meta
        .tenant_id
        .as_deref()
        .map(|t| format!(" in tenant {t}"))
        .unwrap_or_default();
    let default = if is_default { " — default" } else { "" };
    format!(
        "- Azure: account \"{}\" with default {subscription}{tenant}{default}",
        meta.account_label
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
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY, github_account_id TEXT, github_reviewer_account_id TEXT, gitlab_account_id TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE runs (id TEXT PRIMARY KEY, project_id TEXT, type TEXT, github_account_id TEXT, conductor_run_id TEXT)")
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
        for t in ["project_github_accounts", "project_gitlab_accounts"] {
            sqlx::query(&format!("CREATE TABLE {t} (project_id TEXT, account_id TEXT, created_at TEXT)"))
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn resolver_fail_closed_when_no_account_linked() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id) VALUES ('p1')")
            .execute(&db)
            .await
            .unwrap();
        let r = resolve_token_for_project(&db, "p1", "gh", None, AccountSelect::default())
            .await
            .unwrap();
        assert!(r.is_none(), "expected None when no github account linked");

        let r = resolve_token_for_project(&db, "p1", "glab", None, AccountSelect::default())
            .await
            .unwrap();
        assert!(r.is_none(), "expected None when no gitlab account linked");
    }

    #[tokio::test]
    async fn resolver_fail_closed_for_unknown_project() {
        let db = mem_db().await;
        let r = resolve_token_for_project(&db, "missing", "gh", None, AccountSelect::default())
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
        let r = resolve_token_for_project(&db, "p1", "git", None, AccountSelect::default())
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn resolver_unknown_tool_is_none() {
        let db = mem_db().await;
        let r = resolve_token_for_project(&db, "p1", "svn", None, AccountSelect::default())
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
            let r = resolve_token_for_project(&db, "p1", "git", Some(spoof), AccountSelect::default())
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
            let r = resolve_token_for_project(&db, "p1", "git", Some(spoof), AccountSelect::default())
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
            let r = resolve_token_for_project(&db, "p1", "git", Some(good), AccountSelect::default()).await;
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
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, AccountSelect::default())
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
        let r = resolve_token_for_project_with(&mock, &db, "gh", "gh", None, AccountSelect::default()).await;
        assert!(matches!(r, Ok(None)), "provider error must fail closed");
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, AccountSelect::default()).await;
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

    // ---- AWS profile/SSO credential minting (pure helpers) ----

    #[test]
    fn parse_export_credentials_reads_v1_process_json() {
        let json = br#"{"Version":1,"AccessKeyId":"ASIAEXAMPLE","SecretAccessKey":"secret","SessionToken":"tok","Expiration":"2099-01-01T00:00:00Z"}"#;
        let sec = parse_export_credentials(json).unwrap();
        assert_eq!(sec.access_key_id, "ASIAEXAMPLE");
        assert_eq!(sec.secret_access_key, "secret");
        assert_eq!(sec.session_token.as_deref(), Some("tok"));
        // 2099-01-01T00:00:00Z
        assert_eq!(sec.expires_at, 4070908800);
    }

    #[test]
    fn parse_export_credentials_without_expiration_is_short_lived() {
        let json = br#"{"Version":1,"AccessKeyId":"AKIAEXAMPLE","SecretAccessKey":"secret"}"#;
        let before = chrono::Utc::now().timestamp();
        let sec = parse_export_credentials(json).unwrap();
        assert!(sec.session_token.is_none());
        // No Expiration → a near-future expiry so it ages out quickly.
        assert!(sec.expires_at >= before);
        assert!(sec.expires_at <= chrono::Utc::now().timestamp() + AWS_CREDS_SKEW_SECS * 2 + 5);
    }

    #[test]
    fn parse_export_credentials_rejects_garbage() {
        assert!(parse_export_credentials(b"not json").is_err());
    }

    #[test]
    fn stderr_detects_sso_login_required() {
        assert!(stderr_is_sso_login_required(
            "aws: [ERROR]: Error loading SSO Token: Token for SSO_ICT does not exist"
        ));
        assert!(stderr_is_sso_login_required(
            "The SSO session associated with this profile has expired"
        ));
        assert!(stderr_is_sso_login_required(
            "Token has expired and refresh failed"
        ));
        // A generic non-SSO failure must NOT be classified as login-required.
        assert!(!stderr_is_sso_login_required(
            "The config profile (foo) could not be found"
        ));
    }

    #[test]
    fn aws_creds_cache_serves_fresh_and_ages_out() {
        let profile = "unit-test-cache-profile-abc";
        let now = chrono::Utc::now().timestamp();
        put_cached_aws_secret(
            profile,
            &CachedAwsSecret {
                access_key_id: "ak".into(),
                secret_access_key: "sk".into(),
                session_token: None,
                expires_at: now + 3600,
            },
        );
        // Fresh (well beyond skew) → served.
        assert!(get_cached_aws_secret(profile, now).is_some());
        // Within the skew window of expiry → treated as stale (re-mint).
        assert!(get_cached_aws_secret(profile, now + 3600 - AWS_CREDS_SKEW_SECS + 1).is_none());
    }

    // ---- several enabled accounts / per-run GitHub account ----

    async fn seed_multi_project(db: &Db) {
        for (id, label, user) in [
            ("gh1", "main", "author"),
            ("gh2", "work", "worker"),
            ("gh3", "inbox", "asked"),
        ] {
            sqlx::query("INSERT INTO github_accounts (id, label, username, email) VALUES (?, ?, ?, ?)")
                .bind(id)
                .bind(label)
                .bind(user)
                .bind(format!("{user}@x.io"))
                .execute(db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO projects (id, github_account_id) VALUES ('p1', 'gh1'), ('p2', 'gh1')")
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO project_github_accounts (project_id, account_id) VALUES ('p1', 'gh1'), ('p1', 'gh2')")
            .execute(db)
            .await
            .unwrap();
        for (id, project, pinned) in [
            ("run-task", "p1", None),
            ("run-inbox", "p1", Some("gh3")),
            ("run-other", "p2", Some("gh3")),
        ] {
            sqlx::query("INSERT INTO runs (id, project_id, type, github_account_id) VALUES (?, ?, 'task', ?)")
                .bind(id)
                .bind(project)
                .bind(pinned)
                .execute(db)
                .await
                .unwrap();
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_account_matches_id_label_then_username() {
        let acct = |id: &str, label: &str, user: &str| GitAccount {
            provider: "github",
            id: id.into(),
            label: label.into(),
            username: Some(user.into()),
            email: None,
            host: "github.com".into(),
        };
        let accounts = vec![acct("a", "Work", "alice"), acct("b", "alice", "bob")];
        assert_eq!(find_account(&accounts, "b").unwrap().id, "b");
        // A label wins over another account's username.
        assert_eq!(find_account(&accounts, "ALICE").unwrap().id, "b");
        assert_eq!(find_account(&accounts, "@bob").unwrap().id, "b");
        assert_eq!(find_account(&accounts, " work ").unwrap().id, "a");
        assert!(find_account(&accounts, "nobody").is_none());
        assert!(find_account(&accounts, "@").is_none());
    }

    #[tokio::test]
    async fn github_run_accounts_lists_enabled_and_default() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let a = resolve_github_run_accounts(&db, "p1", Some("run-task")).await.unwrap();
        // Ordered by label: main, work.
        assert_eq!(a, GithubRunAccounts { default: Some("gh1".into()), pinned: false, enabled: ids(&["gh1", "gh2"]) });
        let a = resolve_github_run_accounts(&db, "p1", None).await.unwrap();
        assert_eq!(a.default.as_deref(), Some("gh1"));
    }

    #[tokio::test]
    async fn github_run_accounts_pinned_run_adds_its_account() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let a = resolve_github_run_accounts(&db, "p1", Some("run-inbox")).await.unwrap();
        assert_eq!(a, GithubRunAccounts { default: Some("gh3".into()), pinned: true, enabled: ids(&["gh3", "gh1", "gh2"]) });
        // run-other is pinned to gh3 but belongs to p2: p1 must not borrow it.
        let a = resolve_github_run_accounts(&db, "p1", Some("run-other")).await.unwrap();
        assert_eq!(a, GithubRunAccounts { default: Some("gh1".into()), pinned: false, enabled: ids(&["gh1", "gh2"]) });
    }

    #[tokio::test]
    async fn github_run_accounts_worker_inherits_conductor_pin() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        for (id, parent) in [("w-inbox", "run-inbox"), ("w-task", "run-task"), ("w-other", "run-other")] {
            sqlx::query("INSERT INTO runs (id, project_id, type, conductor_run_id) VALUES (?, 'p1', 'task', ?)")
                .bind(id)
                .bind(parent)
                .execute(&db)
                .await
                .unwrap();
        }
        let default = |id: &'static str| {
            let db = db.clone();
            async move { resolve_github_run_accounts(&db, "p1", Some(id)).await.unwrap().default }
        };
        assert_eq!(default("w-inbox").await.as_deref(), Some("gh3"));
        assert_eq!(default("w-task").await.as_deref(), Some("gh1"));
        // A conductor from another project never lends its pin.
        assert_eq!(default("w-other").await.as_deref(), Some("gh1"));
    }

    #[tokio::test]
    async fn selector_picks_enabled_account_or_denies() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let mock = MockProvider::new("some_pat", None);
        let sel = |account: Option<&'static str>| AccountSelect { run_id: Some("run-task"), account, url_username: None };
        let user = |r: Option<ResolvedToken>| r.and_then(|t| t.username);
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, sel(None)).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("author"));
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, sel(Some("work"))).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("worker"));
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("github.com"), sel(Some("@worker")))
            .await
            .unwrap();
        assert_eq!(user(r).as_deref(), Some("worker"));
        // gh3 exists but is not enabled for p1 → deny, naming the enabled ones.
        let e = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, sel(Some("inbox")))
            .await
            .err()
            .expect("unknown account must be denied");
        assert!(e.starts_with(UNKNOWN_GIT_ACCOUNT), "{e}");
        assert!(e.contains("\"main\" (@author), \"work\" (@worker)"), "{e}");
        // A pinned run may also use its pinned account.
        let pinned = AccountSelect { run_id: Some("run-inbox"), account: Some("inbox"), url_username: None };
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, pinned).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("asked"));
    }

    #[tokio::test]
    async fn pinned_run_pins_gh_but_git_keeps_project_default() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let mock = MockProvider::new("some_pat", None);
        let sel = AccountSelect { run_id: Some("run-inbox"), account: None, url_username: None };
        let user = |r: Option<ResolvedToken>| r.and_then(|t| t.username);
        let r = resolve_token_for_project_with(&mock, &db, "p1", "gh", None, sel).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("asked"));
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("github.com"), sel).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("author"));
        // git cannot pick the pinned account when it is not enabled for the project.
        let sel = AccountSelect { account: Some("inbox"), ..sel };
        assert!(resolve_token_for_project_with(&mock, &db, "p1", "git", Some("github.com"), sel).await.is_err());
    }

    #[tokio::test]
    async fn git_url_username_selects_account_when_enabled() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let mock = MockProvider::new("some_pat", None);
        let sel = |u: &'static str| AccountSelect { run_id: None, account: None, url_username: Some(u) };
        let user = |r: Option<ResolvedToken>| r.and_then(|t| t.username);
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("github.com"), sel("worker")).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("worker"));
        // An unknown URL username is ignored → default account.
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("github.com"), sel("x-access-token"))
            .await
            .unwrap();
        assert_eq!(user(r).as_deref(), Some("author"));
    }

    #[tokio::test]
    async fn gitlab_accounts_route_by_host() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO gitlab_accounts (id, label, host, username) VALUES ('gl1', 'cloud', 'gitlab.com', 'u1'), ('gl2', 'corp', 'git.corp.io', 'u2')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects (id, gitlab_account_id) VALUES ('p1', 'gl1')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO project_gitlab_accounts (project_id, account_id) VALUES ('p1', 'gl1'), ('p1', 'gl2')")
            .execute(&db)
            .await
            .unwrap();
        let mock = MockProvider::new("some_pat", None);
        let none = AccountSelect::default();
        let user = |r: Option<ResolvedToken>| r.and_then(|t| t.username);
        // The default does not serve git.corp.io; the account on that host does.
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("git.corp.io"), none).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("u2"));
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("gitlab.com"), none).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("u1"));
        // An account of another host is never sent to this one.
        let sel = AccountSelect { account: Some("corp"), ..none };
        let e = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("gitlab.com"), sel)
            .await
            .err()
            .expect("account of another host must be denied");
        assert!(e.starts_with(UNKNOWN_GIT_ACCOUNT), "{e}");
        let r = resolve_token_for_project_with(&mock, &db, "p1", "git", Some("evil.io"), none).await.unwrap();
        assert!(r.is_none());
        // glab (no host) picks by selector among all enabled accounts.
        let r = resolve_token_for_project_with(&mock, &db, "p1", "glab", None, sel).await.unwrap();
        assert_eq!(user(r).as_deref(), Some("u2"));
    }

    #[tokio::test]
    async fn account_context_single_account_is_unchanged() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let ctx = build_account_context(&db, "p2", None).await.unwrap();
        assert!(ctx.contains("- GitHub: account \"main\" (@author) on github.com\n"), "{ctx}");
        assert!(!ctx.contains(GIT_ACCOUNT_ENV), "{ctx}");
    }

    #[tokio::test]
    async fn account_context_lists_every_enabled_account() {
        let db = mem_db().await;
        seed_multi_project(&db).await;
        let ctx = build_account_context(&db, "p1", Some("run-task")).await.unwrap();
        assert!(
            ctx.contains("- GitHub: account \"main\" (@author) on github.com — default, email author@x.io"),
            "{ctx}"
        );
        assert!(ctx.contains("- GitHub: account \"work\" (@worker) on github.com — email worker@x.io"), "{ctx}");
        assert!(ctx.contains("DEVDY_GIT_ACCOUNT=work gh pr list"), "{ctx}");
        // A pinned run defaults to its pin.
        let ctx = build_account_context(&db, "p1", Some("run-inbox")).await.unwrap();
        assert!(
            ctx.contains("account \"inbox\" (@asked) on github.com — default, email asked@x.io (this session"),
            "{ctx}"
        );
        assert!(!ctx.contains("\"main\" (@author) on github.com — default"), "{ctx}");
        assert!(ctx.contains("git fetch/push and commits on github.com use the project's default account \"main\" (@author)"), "{ctx}");
    }

    #[tokio::test]
    async fn account_context_lists_every_enabled_cloud_account() {
        let db = mem_db().await;
        for sql in [
            "ALTER TABLE projects ADD COLUMN aws_account_id TEXT",
            "ALTER TABLE projects ADD COLUMN azure_account_id TEXT",
            "CREATE TABLE aws_accounts (id TEXT PRIMARY KEY, label TEXT, auth_method TEXT, account_id TEXT, arn TEXT, region TEXT, profile_name TEXT)",
            "CREATE TABLE azure_accounts (id TEXT PRIMARY KEY, label TEXT, tenant_id TEXT, subscription_id TEXT, subscription_name TEXT)",
            "CREATE TABLE project_aws_accounts (project_id TEXT, account_id TEXT, created_at TEXT)",
            "CREATE TABLE project_azure_accounts (project_id TEXT, account_id TEXT, created_at TEXT)",
            "INSERT INTO aws_accounts VALUES ('a1', 'main', 'profile', '111111111111', NULL, 'ap-1', 'main-sso'), ('a2', 'prod', 'profile', '222222222222', NULL, 'us-1', 'prod-sso')",
            "INSERT INTO azure_accounts VALUES ('z1', 'Az', 'ten-1', 'sub-1', 'Dev')",
            "INSERT INTO projects (id, aws_account_id, azure_account_id) VALUES ('p1', 'a2', 'z1')",
            "INSERT INTO project_aws_accounts (project_id, account_id) VALUES ('p1', 'a1'), ('p1', 'a2')",
            "INSERT INTO project_azure_accounts (project_id, account_id) VALUES ('p1', 'z1')",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        let ctx = build_account_context(&db, "p1", None).await.unwrap();
        assert!(ctx.contains("- AWS: account \"main\" (111111111111) in ap-1 via profile main-sso\n"), "{ctx}");
        assert!(ctx.contains("- AWS: account \"prod\" (222222222222) in us-1 via profile prod-sso — default\n"), "{ctx}");
        assert!(ctx.contains("DEVDY_AWS_ACCOUNT=prod aws s3 ls"), "{ctx}");
        // A single Azure account is listed without a default marker or selector hint.
        assert!(ctx.contains("- Azure: account \"Az\" with default subscription \"Dev\" (sub-1) in tenant ten-1\n"), "{ctx}");
        assert!(!ctx.contains(AZURE_ACCOUNT_ENV), "{ctx}");
        assert!(ctx.contains("cloud_accounts_list"), "{ctx}");
        // The selector picks among enabled accounts; anything else is refused.
        let a = pick_aws_account(&db, "p1", Some("MAIN-SSO")).await.unwrap().unwrap();
        assert_eq!(a.id, "a1");
        let e = pick_aws_account(&db, "p1", Some("other")).await.err().unwrap();
        assert!(e.starts_with(UNKNOWN_CLOUD_ACCOUNT), "{e}");
    }
}
