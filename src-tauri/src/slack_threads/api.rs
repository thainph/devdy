//! Minimal Slack Web API client (reqwest). The bearer token is only ever sent
//! to HTTPS `slack.com`, `*.slack.com` or `slack-files.com` (plus an explicit
//! test origin) and never appears in errors or logs.

use reqwest::header::{HeaderMap, AUTHORIZATION, RETRY_AFTER};
use reqwest::StatusCode;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

pub const SLACK_API_BASE: &str = "https://slack.com/api/";
/// Rate-limit retries after the first attempt.
const RATE_LIMIT_RETRIES: u32 = 3;
/// Default / max `Retry-After` wait, in `wait_unit`s.
const DEFAULT_RETRY_AFTER: u64 = 5;
const MAX_RETRY_AFTER: u64 = 60;
/// Network / 5xx backoff, in `wait_unit`s.
const NETWORK_BACKOFF: [u64; 2] = [1, 3];

/// Where and how to talk to Slack. Tests point `api_base` at a local mock and
/// whitelist its origin for the token.
#[derive(Clone)]
pub struct HttpConfig {
    pub api_base: String,
    /// `scheme://host:port` that may receive the token besides Slack hosts
    /// (tests only; `None` in the app).
    pub trusted_test_origin: Option<String>,
    /// 1 s in the app; shortened in tests.
    pub wait_unit: Duration,
    /// Whole-request timeout of Web API calls and avatars.
    pub timeout: Duration,
    /// TCP/TLS connect timeout (all requests).
    pub connect_timeout: Duration,
    /// File downloads: max time without receiving any body bytes.
    pub download_idle_timeout: Duration,
    /// File downloads: overall cap (sized for `files::MAX_FILE_BYTES` on a
    /// slow link).
    pub download_max_duration: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            api_base: SLACK_API_BASE.to_string(),
            trusted_test_origin: None,
            wait_unit: Duration::from_secs(1),
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(15),
            download_idle_timeout: Duration::from_secs(30),
            // 100 MB at ~0.5 Mbit/s.
            download_max_duration: Duration::from_secs(30 * 60),
        }
    }
}

/// `https://<host>…` with host `slack.com`, `*.slack.com`, `slack-files.com`.
pub fn is_token_host(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "https" || u.port().is_some_and(|p| p != 443) {
        return false;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == "slack.com" || host.ends_with(".slack.com") || host == "slack-files.com"
}

impl HttpConfig {
    /// Whether a request to `url` may carry the bearer token.
    pub fn token_allowed(&self, url: &str) -> bool {
        if is_token_host(url) {
            return true;
        }
        match (&self.trusted_test_origin, reqwest::Url::parse(url)) {
            (Some(origin), Ok(u)) => u.origin().ascii_serialization() == *origin,
            _ => false,
        }
    }

    /// Whether a token-less fetch (avatars) may go to `url`: HTTPS only.
    pub fn public_allowed(&self, url: &str) -> bool {
        match reqwest::Url::parse(url) {
            Ok(u) if u.scheme() == "https" => true,
            Ok(u) => self
                .trusted_test_origin
                .as_ref()
                .is_some_and(|o| u.origin().ascii_serialization() == *o),
            Err(_) => false,
        }
    }

    /// Client for token requests: redirects are only followed to hosts that
    /// may receive the token. No client-wide timeout: each request sets its
    /// own (API calls vs. file downloads).
    pub fn token_client(self: &Arc<Self>) -> reqwest::Client {
        let cfg = self.clone();
        reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 || !cfg.token_allowed(attempt.url().as_str()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .unwrap_or_default()
    }

    /// Client for token-less requests.
    pub fn public_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .build()
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlackError {
    /// `{"ok": false, "error": code, "needed"?: …}`
    Api {
        code: String,
        needed: Option<String>,
    },
    /// HTTP 429 after all retries.
    RateLimited,
    /// Connection error / 5xx after all retries.
    Network(String),
    /// Unexpected HTTP status / body.
    Http(String),
}

impl std::fmt::Display for SlackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlackError::Api { code, needed } => match needed {
                Some(n) => write!(f, "{code} (needed: {n})"),
                None => f.write_str(code),
            },
            SlackError::RateLimited => f.write_str("rate limited"),
            SlackError::Network(e) | SlackError::Http(e) => f.write_str(e),
        }
    }
}

/// reqwest errors can embed the URL (with query params) — keep only the kind.
fn net_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "request timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else {
        "request failed".into()
    }
}

/// `auth.test` result.
#[derive(Debug, Clone)]
pub struct AuthInfo {
    pub team_id: String,
    pub team: String,
    /// Lowercased host of the workspace URL.
    pub host: String,
    /// From the `x-oauth-scopes` header; `None` when the header is absent.
    pub scopes: Option<HashSet<String>>,
}

impl AuthInfo {
    /// `Some(true/false)` when scopes are known, `None` otherwise.
    pub fn has_scope(&self, scope: &str) -> Option<bool> {
        self.scopes.as_ref().map(|s| s.contains(scope))
    }

    /// Known to lack every scope in `any_of`.
    pub fn lacks_all(&self, any_of: &[&str]) -> bool {
        self.scopes
            .as_ref()
            .is_some_and(|s| any_of.iter().all(|x| !s.contains(*x)))
    }
}

/// Parse `x-oauth-scopes: a,b, c`.
pub fn parse_scopes(header: &str) -> HashSet<String> {
    header
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ChannelInfo {
    pub name: Option<String>,
    pub is_im: bool,
    pub is_mpim: bool,
    pub is_private: bool,
}

impl ChannelInfo {
    pub fn kind(&self) -> &'static str {
        if self.is_im {
            "im"
        } else if self.is_mpim {
            "mpim"
        } else if self.is_private {
            "private"
        } else {
            "public"
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UserInfo {
    pub display_name: Option<String>,
    pub real_name: Option<String>,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub is_bot: bool,
}

fn non_empty(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Authenticated Slack client. Deliberately not `Debug` (holds the token).
#[derive(Clone)]
pub struct SlackClient {
    cfg: Arc<HttpConfig>,
    http: reqwest::Client,
    token: String,
}

impl SlackClient {
    pub fn new(cfg: Arc<HttpConfig>, http: reqwest::Client, token: String) -> Self {
        SlackClient { cfg, http, token }
    }

    fn wait(&self, units: u64) -> Duration {
        self.cfg.wait_unit * units as u32
    }

    /// Max wait for the next chunk of a file download.
    pub fn download_idle_timeout(&self) -> Duration {
        self.cfg.download_idle_timeout
    }

    /// GET a Web API URL with the token (whole request ≤ `timeout`).
    pub async fn get_with_token(&self, url: &str) -> Result<reqwest::Response, SlackError> {
        self.get_with_token_within(url, self.cfg.timeout).await
    }

    /// GET a file with the token. The overall cap is generous; callers read
    /// the body with `download_idle_timeout` per chunk.
    pub async fn get_download(&self, url: &str) -> Result<reqwest::Response, SlackError> {
        self.get_with_token_within(url, self.cfg.download_max_duration)
            .await
    }

    /// GET with the token, retrying 429 (Retry-After) and network/5xx errors.
    async fn get_with_token_within(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<reqwest::Response, SlackError> {
        if !self.cfg.token_allowed(url) {
            return Err(SlackError::Http(
                "refusing to send the token to this host".into(),
            ));
        }
        let mut rate_retries = 0;
        let mut net_retries = 0;
        loop {
            let res = self
                .http
                .get(url)
                .timeout(timeout)
                .header(AUTHORIZATION, format!("Bearer {}", self.token))
                .send()
                .await;
            match res {
                Ok(r) if r.status() == StatusCode::TOO_MANY_REQUESTS => {
                    if rate_retries >= RATE_LIMIT_RETRIES {
                        return Err(SlackError::RateLimited);
                    }
                    rate_retries += 1;
                    let secs = r
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .unwrap_or(DEFAULT_RETRY_AFTER)
                        .min(MAX_RETRY_AFTER);
                    tokio::time::sleep(self.wait(secs)).await;
                }
                Ok(r) if r.status().is_server_error() => {
                    if net_retries >= NETWORK_BACKOFF.len() {
                        return Err(SlackError::Network(format!("HTTP {}", r.status().as_u16())));
                    }
                    tokio::time::sleep(self.wait(NETWORK_BACKOFF[net_retries])).await;
                    net_retries += 1;
                }
                Ok(r) => return Ok(r),
                Err(e) => {
                    if net_retries >= NETWORK_BACKOFF.len() {
                        return Err(SlackError::Network(net_error(&e)));
                    }
                    tokio::time::sleep(self.wait(NETWORK_BACKOFF[net_retries])).await;
                    net_retries += 1;
                }
            }
        }
    }

    /// Call a Web API method (GET + query); `ok:false` → `SlackError::Api`.
    async fn call(
        &self,
        method: &str,
        params: &[(&str, &str)],
    ) -> Result<(Value, HeaderMap), SlackError> {
        let mut url = reqwest::Url::parse(&self.cfg.api_base)
            .and_then(|b| b.join(method))
            .map_err(|_| SlackError::Http("invalid API base URL".into()))?;
        if !params.is_empty() {
            url.query_pairs_mut().extend_pairs(params.iter().copied());
        }
        let resp = self.get_with_token(url.as_str()).await?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body: Value = resp.json().await.map_err(|_| {
            SlackError::Http(format!(
                "{method}: invalid response (HTTP {})",
                status.as_u16()
            ))
        })?;
        if body.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
                .to_string();
            let needed = non_empty(body.get("needed"));
            return Err(SlackError::Api { code, needed });
        }
        Ok((body, headers))
    }

    /// POST a Web API method with a JSON body. Only HTTP 429 is retried (the
    /// message was not accepted); network errors / 5xx are NOT retried, since
    /// the call may have gone through and a retry could post twice.
    async fn post_json(&self, method: &str, body: &Value) -> Result<Value, SlackError> {
        let url = reqwest::Url::parse(&self.cfg.api_base)
            .and_then(|b| b.join(method))
            .map_err(|_| SlackError::Http("invalid API base URL".into()))?;
        if !self.cfg.token_allowed(url.as_str()) {
            return Err(SlackError::Http(
                "refusing to send the token to this host".into(),
            ));
        }
        let mut rate_retries = 0;
        let resp = loop {
            let r = self
                .http
                .post(url.clone())
                .timeout(self.cfg.timeout)
                .header(AUTHORIZATION, format!("Bearer {}", self.token))
                .json(body)
                .send()
                .await
                .map_err(|e| SlackError::Network(net_error(&e)))?;
            if r.status() != StatusCode::TOO_MANY_REQUESTS {
                break r;
            }
            if rate_retries >= RATE_LIMIT_RETRIES {
                return Err(SlackError::RateLimited);
            }
            rate_retries += 1;
            let secs = r
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(DEFAULT_RETRY_AFTER)
                .min(MAX_RETRY_AFTER);
            tokio::time::sleep(self.wait(secs)).await;
        };
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|_| {
            SlackError::Http(format!(
                "{method}: invalid response (HTTP {})",
                status.as_u16()
            ))
        })?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
                .to_string();
            let needed = non_empty(v.get("needed"));
            return Err(SlackError::Api { code, needed });
        }
        Ok(v)
    }

    /// `chat.postMessage` as a thread reply; returns the new message `ts`.
    pub async fn post_thread_reply(
        &self,
        channel: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<String, SlackError> {
        let body = serde_json::json!({
            "channel": channel,
            "thread_ts": thread_ts,
            "text": text,
        });
        let v = self.post_json("chat.postMessage", &body).await?;
        Ok(non_empty(v.get("ts")).unwrap_or_default())
    }

    pub async fn auth_test(&self) -> Result<AuthInfo, SlackError> {
        let (v, headers) = self.call("auth.test", &[]).await?;
        let url = non_empty(v.get("url")).unwrap_or_default();
        let host = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        let scopes = headers
            .get("x-oauth-scopes")
            .and_then(|h| h.to_str().ok())
            .map(parse_scopes);
        Ok(AuthInfo {
            team_id: non_empty(v.get("team_id")).unwrap_or_default(),
            team: non_empty(v.get("team")).unwrap_or_default(),
            host,
            scopes,
        })
    }

    pub async fn conversations_info(&self, channel: &str) -> Result<ChannelInfo, SlackError> {
        let (v, _) = self
            .call("conversations.info", &[("channel", channel)])
            .await?;
        let c = v.get("channel").cloned().unwrap_or(Value::Null);
        let flag = |k: &str| c.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(ChannelInfo {
            name: non_empty(c.get("name")),
            is_im: flag("is_im"),
            is_mpim: flag("is_mpim"),
            is_private: flag("is_private") || flag("is_group"),
        })
    }

    /// Every message of a thread (parent first), following `next_cursor`, at
    /// most `max` messages. Returns `(messages, truncated)`.
    pub async fn conversations_replies_all(
        &self,
        channel: &str,
        ts: &str,
        max: usize,
    ) -> Result<(Vec<Value>, bool), SlackError> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        loop {
            let mut params = vec![
                ("channel", channel),
                ("ts", ts),
                ("limit", "200"),
                ("inclusive", "true"),
            ];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.as_str()));
            }
            let (v, _) = self.call("conversations.replies", &params).await?;
            if let Some(msgs) = v.get("messages").and_then(Value::as_array) {
                for m in msgs {
                    if out.len() >= max {
                        return Ok((out, true));
                    }
                    out.push(m.clone());
                }
            }
            let next = v
                .pointer("/response_metadata/next_cursor")
                .and_then(Value::as_str)
                .unwrap_or("");
            if next.is_empty() {
                return Ok((out, false));
            }
            cursor = next.to_string();
        }
    }

    pub async fn users_info(&self, user: &str) -> Result<UserInfo, SlackError> {
        let (v, _) = self.call("users.info", &[("user", user)]).await?;
        let u = v.get("user").cloned().unwrap_or(Value::Null);
        Ok(UserInfo {
            display_name: non_empty(u.pointer("/profile/display_name")),
            real_name: non_empty(u.pointer("/profile/real_name"))
                .or_else(|| non_empty(u.get("real_name"))),
            name: non_empty(u.get("name")),
            avatar_url: non_empty(u.pointer("/profile/image_72")),
            is_bot: u.get("is_bot").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hosts() {
        assert!(is_token_host(
            "https://files.slack.com/files-pri/T1-F1/a.png"
        ));
        assert!(is_token_host("https://slack.com/api/auth.test"));
        assert!(is_token_host("https://slack-files.com/T1-F1-abc"));
        assert!(!is_token_host("http://files.slack.com/x"));
        assert!(!is_token_host("https://files.slack.com.evil.com/x"));
        assert!(!is_token_host("https://evilslack.com/x"));
        assert!(!is_token_host("https://avatars.slack-edge.com/x.png"));
        assert!(!is_token_host("https://files.slack.com:8443/x"));
        let cfg = HttpConfig {
            trusted_test_origin: Some("http://127.0.0.1:5555".into()),
            ..Default::default()
        };
        assert!(cfg.token_allowed("http://127.0.0.1:5555/api/x"));
        assert!(!cfg.token_allowed("http://127.0.0.1:5556/api/x"));
        assert!(!HttpConfig::default().token_allowed("http://127.0.0.1:5555/api/x"));
        assert!(HttpConfig::default().public_allowed("https://avatars.slack-edge.com/x.png"));
        assert!(!HttpConfig::default().public_allowed("http://avatars.slack-edge.com/x.png"));
    }

    #[test]
    fn scopes_header() {
        let s = parse_scopes("channels:history, groups:history,files:read,");
        assert_eq!(s.len(), 3);
        let a = AuthInfo {
            team_id: "T".into(),
            team: "t".into(),
            host: String::new(),
            scopes: Some(s),
        };
        assert_eq!(a.has_scope("files:read"), Some(true));
        assert_eq!(a.has_scope("users:read"), Some(false));
        assert!(a.lacks_all(&["channels:read", "groups:read"]));
        let unknown = AuthInfo { scopes: None, ..a };
        assert_eq!(unknown.has_scope("files:read"), None);
        assert!(!unknown.lacks_all(&["channels:read"]));
    }
}
