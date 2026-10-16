//! Slack token lookup: MCP servers whose env holds `SLACK_MCP_XOXP_TOKEN`
//! (VALUE in the Keychain), workspace matching via `auth.test` and the scope
//! list from its `x-oauth-scopes` header.

use super::api::{AuthInfo, HttpConfig, SlackClient, SlackError};
use super::CodedError;
use crate::db::Db;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TOKEN_ENV_KEY: &str = "SLACK_MCP_XOXP_TOKEN";
const AUTH_CACHE_TTL: Duration = Duration::from_secs(3600);

/// `server_id → token value` (the Keychain in the app, a map in tests).
pub type SecretLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Reads the token VALUE of an MCP server from the Keychain store.
pub fn keychain_lookup() -> SecretLookup {
    Arc::new(|server_id: &str| {
        crate::secrets::get_mcp_secrets(server_id)
            .env
            .get(TOKEN_ENV_KEY)
            .cloned()
    })
}

/// A usable token and the MCP server it came from. Not `Debug` on purpose.
#[derive(Clone)]
pub struct TokenSource {
    pub server_id: String,
    pub server_name: String,
    token: String,
}

impl TokenSource {
    fn cache_key(&self) -> String {
        format!(
            "{}:{}",
            self.server_id,
            hex::encode(Sha256::digest(self.token.as_bytes()))
        )
    }
}

fn looks_like_token(v: &str) -> bool {
    let v = v.trim();
    v.starts_with("xoxp-") || v.starts_with("xoxb-")
}

/// Every MCP server (oldest first, `enabled` ignored) with a usable token.
pub async fn sources(db: &Db, lookup: &SecretLookup) -> Result<Vec<TokenSource>, sqlx::Error> {
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, name, env_keys FROM mcp_servers ORDER BY created_at ASC, id ASC",
    )
    .fetch_all(db)
    .await?;
    let candidates: Vec<(String, String)> = rows
        .into_iter()
        .filter(|(_, _, keys)| {
            keys.as_deref()
                .and_then(|k| serde_json::from_str::<Vec<String>>(k).ok())
                .is_some_and(|k| k.iter().any(|x| x == TOKEN_ENV_KEY))
        })
        .map(|(id, name, _)| (id, name))
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    // The Keychain read may block (first access), keep it off the runtime.
    let lookup = lookup.clone();
    let found = tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter_map(|(id, name)| {
                let token = lookup(&id)?.trim().to_string();
                looks_like_token(&token).then_some(TokenSource {
                    server_id: id,
                    server_name: name,
                    token,
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    Ok(found)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenSourceInfo {
    pub server_id: String,
    pub server_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SlackTokenStatus {
    /// `ok` | `none` | `multiple`
    pub state: &'static str,
    pub sources: Vec<TokenSourceInfo>,
}

/// Token availability without any network call.
pub async fn token_status(db: &Db, lookup: &SecretLookup) -> Result<SlackTokenStatus, sqlx::Error> {
    let found = sources(db, lookup).await?;
    Ok(SlackTokenStatus {
        state: match found.len() {
            0 => "none",
            1 => "ok",
            _ => "multiple",
        },
        sources: found
            .into_iter()
            .map(|s| TokenSourceInfo {
                server_id: s.server_id,
                server_name: s.server_name,
            })
            .collect(),
    })
}

/// In-memory `auth.test` cache keyed by `server_id + sha256(token)`.
#[derive(Clone, Default)]
pub struct AuthCache(Arc<Mutex<HashMap<String, (Instant, AuthInfo)>>>);

impl AuthCache {
    fn get(&self, key: &str) -> Option<AuthInfo> {
        let g = self.0.lock().ok()?;
        g.get(key)
            .filter(|(at, _)| at.elapsed() < AUTH_CACHE_TTL)
            .map(|(_, a)| a.clone())
    }

    #[cfg(test)]
    pub fn clear(&self) {
        if let Ok(mut g) = self.0.lock() {
            g.clear();
        }
    }

    fn put(&self, key: String, info: AuthInfo) {
        if let Ok(mut g) = self.0.lock() {
            g.insert(key, (Instant::now(), info));
        }
    }
}

/// Fatal mapping for `auth.test` failures.
pub fn auth_error(e: SlackError) -> CodedError {
    match e {
        SlackError::Api { code, .. } => match code.as_str() {
            "invalid_auth" | "not_authed" | "token_revoked" | "token_expired"
            | "account_inactive" => CodedError::new("invalid_auth", code),
            _ => CodedError::new("slack_error", code),
        },
        SlackError::RateLimited => CodedError::new("rate_limited", "rate limited by Slack"),
        SlackError::Network(e) => CodedError::new("network", e),
        SlackError::Http(e) => CodedError::new("slack_error", e),
    }
}

/// Pick the token for `host` and return a client plus its `auth.test` info.
/// One token → used as is; several → the one whose `auth.test` URL host is
/// `host` (earliest server wins).
pub async fn resolve(
    found: &[TokenSource],
    host: &str,
    cfg: &Arc<HttpConfig>,
    http: &reqwest::Client,
    cache: &AuthCache,
) -> Result<(SlackClient, AuthInfo), CodedError> {
    if found.is_empty() {
        return Err(CodedError::new("no_token", "no Slack token configured"));
    }
    for src in found {
        let client = SlackClient::new(cfg.clone(), http.clone(), src.token.clone());
        let key = src.cache_key();
        let info = match cache.get(&key) {
            Some(i) => i,
            None => match client.auth_test().await {
                Ok(i) => {
                    cache.put(key, i.clone());
                    i
                }
                // With several tokens a failing one simply doesn't match.
                Err(e) if found.len() == 1 => return Err(auth_error(e)),
                Err(_) => continue,
            },
        };
        if found.len() == 1 || info.host == host {
            return Ok((client, info));
        }
    }
    Err(CodedError::new(
        "no_token_for_workspace",
        format!("no Slack token for {host}"),
    ))
}
