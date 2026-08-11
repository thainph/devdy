//! Google OAuth (Drive + Gmail) connection management.
//!
//! The user pastes their OWN OAuth Desktop-app credentials (QĐ: tự dán). We run
//! the standard loopback authorization-code flow: open the consent screen in the
//! browser, catch the redirect on a throwaway `127.0.0.1:<port>` listener, and
//! exchange the code for a long-lived `refresh_token`. Only the refresh_token
//! (plus the client id/secret and account email) is persisted — into the same
//! consolidated Keychain item as everything else (see `secrets.rs`).
//!
//! Access tokens are intentionally NOT stored here: the built-in `gdrive` /
//! `gmail` MCP servers receive the refresh_token via env and mint their own
//! short-lived access tokens on demand, so a run that outlives the 1h access
//! token keeps working.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::db::Db;
use crate::secrets;

/// Scopes requested in a single consent: full Drive, Gmail modify (read/send/
/// label/trash — not permanent delete of the mailbox), plus email for display.
const SCOPES: &str = "https://www.googleapis.com/auth/drive \
https://www.googleapis.com/auth/gmail.modify \
https://www.googleapis.com/auth/calendar.events \
https://www.googleapis.com/auth/userinfo.email openid";

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v2/userinfo";

/// One connected Google account (metadata; the refresh_token stays in Keychain).
#[derive(Serialize)]
pub struct GoogleAccount {
    pub id: String,
    pub label: String,
    pub email: String,
    pub scope: String,
    pub is_default: bool,
    pub created_at: String,
    /// Computed from `scope`: true when the granted scopes include calendar
    /// write access (`calendar.events` or full `calendar`). Lets the FE gate
    /// write actions without re-parsing the scope string.
    pub calendar_writable: bool,
}

/// Whether the granted scope string includes calendar write access.
fn scope_has_calendar_write(scope: &str) -> bool {
    scope.split_whitespace().any(|s| {
        s == "https://www.googleapis.com/auth/calendar.events"
            || s == "https://www.googleapis.com/auth/calendar"
    })
}

/// Whether reusable OAuth client credentials are saved (so adding an account
/// needs only a consent, no re-entry).
#[derive(Serialize)]
pub struct GoogleClientStatus {
    pub has_client: bool,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    #[serde(default)]
    email: String,
}

/// Percent-encode a query-string VALUE (RFC 3986 unreserved kept as-is).
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Percent-decode a query-string value (`+` → space, `%XX` → byte).
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pull `key=value` pairs out of a `?a=1&b=2` query string.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k == key {
            Some(pct_decode(it.next().unwrap_or("")))
        } else {
            None
        }
    })
}

/// Minimal HTML shown in the browser after the redirect lands.
fn done_page(ok: bool) -> String {
    let (title, body) = if ok {
        ("Connected", "Google account connected. You can close this tab and return to Devdy.")
    } else {
        ("Failed", "Authorization failed or was cancelled. You can close this tab.")
    };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
<style>body{{font-family:-apple-system,system-ui,sans-serif;background:#0b0b12;color:#e5e7eb;\
display:flex;align-items:center;justify-content:center;height:100vh;margin:0}}\
div{{text-align:center;max-width:420px;padding:24px}}h1{{font-size:18px;margin:0 0 8px}}\
p{{color:#9ca3af;font-size:14px;line-height:1.5}}</style></head>\
<body><div><h1>{title}</h1><p>{body}</p></div></body></html>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.as_bytes().len(),
        html
    )
}

/// Read one HTTP request off the socket and return its request-target path+query
/// (e.g. `/?code=...&state=...`). Reads a single chunk — enough for a bare GET.
async fn read_request_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).await.ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let first = head.lines().next()?;
    // "GET /?code=... HTTP/1.1"
    let mut parts = first.split_whitespace();
    let _method = parts.next()?;
    Some(parts.next()?.to_string())
}

/// Result of a completed OAuth consent flow.
struct OauthResult {
    refresh_token: String,
    email: String,
    scope: String,
}

/// Run the loopback OAuth consent flow with the given client creds. Opens the
/// browser, catches the `127.0.0.1` redirect, exchanges the code, and resolves
/// the account email. Returns the refresh_token + email + granted scopes.
async fn run_oauth(
    app: &AppHandle,
    client_id: &str,
    client_secret: &str,
) -> Result<OauthResult, String> {
    // Throwaway loopback listener on a random free port.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to open loopback listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read loopback port: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let state = Uuid::new_v4().to_string();

    let auth_url = format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        pct_encode(client_id),
        pct_encode(&redirect_uri),
        pct_encode(SCOPES),
        pct_encode(&state),
    );

    app.opener()
        .open_url(auth_url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {e}"))?;

    // Wait for the redirect (5 min budget). Loop so favicon / preflight hits
    // don't consume the real callback.
    let target = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let target = read_request_target(&mut stream).await;
            match target {
                Some(t) if t.contains("code=") || t.contains("error=") => {
                    let ok = t.contains("code=");
                    let _ = stream.write_all(done_page(ok).as_bytes()).await;
                    let _ = stream.flush().await;
                    return Some(t);
                }
                _ => {
                    let _ = stream.write_all(done_page(false).as_bytes()).await;
                    let _ = stream.flush().await;
                }
            }
        }
    })
    .await
    .map_err(|_| "Timed out waiting for Google authorization (5 min).".to_string())?
    .ok_or_else(|| "No authorization response received.".to_string())?;

    let query = target.splitn(2, '?').nth(1).unwrap_or("");
    if let Some(err) = query_param(query, "error") {
        return Err(format!("Authorization denied: {err}"));
    }
    if query_param(query, "state").as_deref() != Some(state.as_str()) {
        return Err("State mismatch — possible CSRF, aborting.".into());
    }
    let code = query_param(query, "code")
        .ok_or_else(|| "No authorization code in redirect.".to_string())?;

    // Exchange the code for tokens.
    let client = reqwest::Client::new();
    let token: TokenResponse = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Token exchange rejected: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    let refresh_token = token
        .refresh_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            "Google did not return a refresh_token. Revoke Devdy's access in your Google account \
             and reconnect (consent must re-prompt)."
                .to_string()
        })?;

    // Resolve the account email for display (best-effort).
    let email = match client
        .get(USERINFO_ENDPOINT)
        .bearer_auth(&token.access_token)
        .send()
        .await
        .ok()
        .and_then(|r| r.error_for_status().ok())
    {
        Some(r) => r.json::<UserInfo>().await.map(|u| u.email).unwrap_or_default(),
        None => String::new(),
    };

    Ok(OauthResult {
        refresh_token,
        email,
        scope: token.scope.unwrap_or_else(|| SCOPES.to_string()),
    })
}

fn row_to_account(row: &sqlx::sqlite::SqliteRow) -> GoogleAccount {
    let scope: String = row.try_get("scope").unwrap_or_default();
    GoogleAccount {
        id: row.get("id"),
        label: row.get("label"),
        email: row.try_get("email").unwrap_or_default(),
        calendar_writable: scope_has_calendar_write(&scope),
        scope,
        is_default: row.get::<i64, _>("is_default") != 0,
        created_at: row.get("created_at"),
    }
}

/// One-time migration: an account connected under the old single-account design
/// is moved into the `google_accounts` table + Keychain map. No-op otherwise.
async fn migrate_legacy(db: &Db) {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM google_accounts")
        .fetch_one(db)
        .await
        .unwrap_or(0);
    if count > 0 {
        return;
    }
    if let Some(legacy) = secrets::take_legacy_google() {
        // Ensure the shared client is saved (older builds stored it per-account).
        if secrets::get_google_client().is_none() {
            let _ = secrets::set_google_client(&legacy.client_id, &legacy.client_secret);
        }
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let label = if legacy.email.is_empty() {
            "default".to_string()
        } else {
            legacy.email.clone()
        };
        if sqlx::query(
            "INSERT INTO google_accounts (id, label, email, scope, is_default, created_at) \
             VALUES (?, ?, ?, ?, 1, ?)",
        )
        .bind(&id)
        .bind(&label)
        .bind(&legacy.email)
        .bind(&legacy.scope)
        .bind(&now)
        .execute(db)
        .await
        .is_ok()
        {
            let _ = secrets::set_google_account_token(&id, &legacy.refresh_token);
        }
    }
}

/// List connected Google accounts (migrating any legacy single account first).
#[tauri::command]
pub async fn list_google_accounts(db: State<'_, Db>) -> Result<Vec<GoogleAccount>, String> {
    migrate_legacy(db.inner()).await;
    let rows = sqlx::query(
        "SELECT id, label, email, scope, is_default, created_at FROM google_accounts \
         ORDER BY is_default DESC, label",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_account).collect())
}

/// Add a Google account. Reuses the saved OAuth client when `client_id`/
/// `client_secret` are omitted (so extra accounts need no re-entry). The first
/// account added becomes the default.
#[tauri::command]
pub async fn add_google_account(
    app: AppHandle,
    db: State<'_, Db>,
    label: String,
    client_id: Option<String>,
    client_secret: Option<String>,
) -> Result<GoogleAccount, String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("A label is required.".into());
    }

    // Resolve client creds: saved unless a fresh pair is entered.
    let entered_id = client_id.unwrap_or_default().trim().to_string();
    let entered_secret = client_secret.unwrap_or_default().trim().to_string();
    let (client_id, client_secret) = if entered_id.is_empty() && entered_secret.is_empty() {
        match secrets::get_google_client() {
            Some(c) => (c.client_id, c.client_secret),
            None => {
                return Err(
                    "No saved OAuth credentials. Enter your Client ID and Client Secret.".into(),
                )
            }
        }
    } else {
        (entered_id, entered_secret)
    };
    if client_id.is_empty() || client_secret.is_empty() {
        return Err("Client ID and Client Secret are required.".into());
    }
    // Persist the client for reuse even if the consent is cancelled.
    secrets::set_google_client(&client_id, &client_secret)
        .map_err(|e| format!("Failed to save credentials: {e}"))?;

    let oauth = run_oauth(&app, &client_id, &client_secret).await?;

    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM google_accounts")
        .fetch_one(db.inner())
        .await
        .unwrap_or(0);
    let is_default = if existing == 0 { 1 } else { 0 };

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO google_accounts (id, label, email, scope, is_default, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&label)
    .bind(&oauth.email)
    .bind(&oauth.scope)
    .bind(is_default)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            format!("An account labelled '{label}' already exists.")
        } else {
            e.to_string()
        }
    })?;

    secrets::set_google_account_token(&id, &oauth.refresh_token)
        .map_err(|e| format!("Failed to store token: {e}"))?;

    Ok(GoogleAccount {
        id,
        label,
        email: oauth.email,
        calendar_writable: scope_has_calendar_write(&oauth.scope),
        scope: oauth.scope,
        is_default: is_default != 0,
        created_at: now,
    })
}

/// Remove a Google account. If it was the default and others remain, the
/// oldest remaining account is promoted to default.
#[tauri::command]
pub async fn delete_google_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    let was_default: Option<i64> =
        sqlx::query_scalar("SELECT is_default FROM google_accounts WHERE id = ?")
            .bind(&id)
            .fetch_optional(db.inner())
            .await
            .map_err(|e| e.to_string())?;

    let _ = secrets::delete_google_account_token(&id);
    sqlx::query("DELETE FROM google_accounts WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    if was_default == Some(1) {
        if let Some(next_id) =
            sqlx::query_scalar::<_, String>("SELECT id FROM google_accounts ORDER BY created_at LIMIT 1")
                .fetch_optional(db.inner())
                .await
                .map_err(|e| e.to_string())?
        {
            sqlx::query("UPDATE google_accounts SET is_default = 1 WHERE id = ?")
                .bind(&next_id)
                .execute(db.inner())
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Rename an account's label.
#[tauri::command]
pub async fn rename_google_account(
    db: State<'_, Db>,
    id: String,
    label: String,
) -> Result<(), String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("A label is required.".into());
    }
    sqlx::query("UPDATE google_accounts SET label = ? WHERE id = ?")
        .bind(&label)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                format!("An account labelled '{label}' already exists.")
            } else {
                e.to_string()
            }
        })?;
    Ok(())
}

/// Mark one account as the default (used when a tool call omits `account`).
#[tauri::command]
pub async fn set_default_google_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    sqlx::query("UPDATE google_accounts SET is_default = 0")
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE google_accounts SET is_default = 1 WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether reusable OAuth client credentials are saved.
#[tauri::command]
pub async fn google_client_status() -> Result<GoogleClientStatus, String> {
    Ok(GoogleClientStatus {
        has_client: secrets::has_google_client(),
    })
}

/// Forget the saved OAuth client credentials AND remove every account (they
/// can't be refreshed without the client).
#[tauri::command]
pub async fn google_forget_client(db: State<'_, Db>) -> Result<(), String> {
    sqlx::query("DELETE FROM google_accounts")
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    secrets::delete_google_client().map_err(|e| format!("Failed to clear credentials: {e}"))
}
