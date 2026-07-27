//! Devdy Remote Control Relay — binary entrypoint.
//!
//! Reads configuration from the environment (see `config.rs` / README), binds
//! `RELAY_BIND` (default `127.0.0.1:8787`), and serves until SIGINT/SIGTERM.

use std::process::ExitCode;

use devdy_relay::config::Config;
use devdy_relay::server;

#[tokio::main]
async fn main() -> ExitCode {
    // Structured logging; metadata only, never payload (NFR-005).
    // Level controlled by RUST_LOG (default: info).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            // Do not print the token; the error type never contains it.
            tracing::error!(event = "config_error", error = %e);
            eprintln!("relay: configuration error: {e}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        event = "relay_start",
        bind = %config.bind,
        pair_ttl_secs = config.pair_ttl.as_secs(),
        "starting devdy relay"
    );

    let (handle, accept_loop) = match server::serve(config).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(event = "bind_error", error = %e);
            eprintln!("relay: failed to bind: {e}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(event = "relay_ready", addr = %handle.local_addr);

    // Run the accept loop until a shutdown signal arrives.
    tokio::select! {
        _ = accept_loop => {},
        _ = shutdown_signal() => {
            tracing::info!(event = "shutdown", "signal received, stopping relay");
        }
    }

    ExitCode::SUCCESS
}

/// Resolve on SIGINT (Ctrl-C) or, on Unix, SIGTERM (systemd stop).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
