//! Relay configuration loaded from environment variables.
//!
//! No secret is ever hardcoded (SEC-001). The host auth token MUST be provided
//! via `RELAY_HOST_TOKEN`; the process refuses to start without it.

use std::time::Duration;

/// Errors raised while loading configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// A required variable was missing or empty.
    Missing(&'static str),
    /// A variable held a value that could not be parsed.
    Invalid {
        key: &'static str,
        reason: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Missing(key) => {
                write!(f, "required environment variable `{key}` is missing or empty")
            }
            ConfigError::Invalid { key, reason } => {
                write!(f, "environment variable `{key}` is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Fully resolved relay configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address to bind, e.g. `127.0.0.1:8787` (Topology B: localhost only).
    pub bind: String,
    /// Shared secret used to authenticate Host `register_host` frames.
    pub host_token: String,
    /// Time-to-live for an unused `pair_code`.
    pub pair_ttl: Duration,
    /// Maximum control frames a single connection may send per window.
    pub rate_limit_max: u32,
    /// Rate-limit sliding window.
    pub rate_limit_window: Duration,
    /// Idle timeout for an ACTIVE room with no traffic before it is closed.
    pub room_idle_timeout: Duration,
    /// Grace window kept for an ACTIVE room after its Controller drops, during
    /// which the Controller may `resume` the same room (phone screen off/on).
    pub reconnect_window: Duration,
}

impl Config {
    /// Default bind address when `RELAY_BIND` is unset (localhost, port 8787).
    pub const DEFAULT_BIND: &'static str = "127.0.0.1:8787";

    /// Load configuration from process environment.
    ///
    /// Fails closed if `RELAY_HOST_TOKEN` is absent so the relay never runs
    /// with an implicit/empty secret.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = env_or("RELAY_BIND", Self::DEFAULT_BIND);

        let host_token = match std::env::var("RELAY_HOST_TOKEN") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Err(ConfigError::Missing("RELAY_HOST_TOKEN")),
        };

        let pair_ttl = Duration::from_secs(parse_u64("PAIR_TTL_SECS", 60)?);
        let rate_limit_max = parse_u64("RELAY_RATE_LIMIT_MAX", 60)? as u32;
        let rate_limit_window =
            Duration::from_secs(parse_u64("RELAY_RATE_LIMIT_WINDOW_SECS", 10)?);
        let room_idle_timeout =
            Duration::from_secs(parse_u64("RELAY_ROOM_IDLE_TIMEOUT_SECS", 3600)?);
        let reconnect_window =
            Duration::from_secs(parse_u64("RELAY_RECONNECT_WINDOW_SECS", 3600)?);

        Ok(Config {
            bind,
            host_token,
            pair_ttl,
            rate_limit_max,
            rate_limit_window,
            room_idle_timeout,
            reconnect_window,
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

fn parse_u64(key: &'static str, default: u64) -> Result<u64, ConfigError> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().parse::<u64>().map_err(|e| ConfigError::Invalid {
            key,
            reason: e.to_string(),
        }),
        _ => Ok(default),
    }
}
