//! The Controller's Web Push actions (`push_*`). Reached only through
//! [`crate::remote::handler::handle`], after the allow-list, the rate limit
//! and the `push` feature gate. Every outcome is audited exactly once.

use super::{send, store};
use crate::remote::command::RejectReason;
use crate::remote::handler::{accept, command_ack, reject, send_payload, HandlerCtx};
use crate::remote::protocol::{CmdPayload, StreamPayload};
use tauri::Manager;

/// Longest device label kept.
pub const MAX_LABEL_CHARS: usize = 64;

/// A validated `push_subscribe`.
#[derive(Debug, PartialEq, Eq)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
}

/// Validate a `push_subscribe`: a known push service, and keys that decode to
/// a P-256 point and a 16-byte secret.
pub fn validate_subscribe(cmd: &CmdPayload) -> Result<NewSubscription, RejectReason> {
    let endpoint = validate_endpoint(cmd)?;
    let p256dh = cmd.p256dh.as_deref().map(str::trim).unwrap_or("");
    let auth = cmd.auth.as_deref().map(str::trim).unwrap_or("");
    if super::crypto::parse_public_key(p256dh).is_none() || super::crypto::parse_auth_secret(auth).is_none() {
        return Err(RejectReason::Malformed);
    }
    Ok(NewSubscription {
        endpoint,
        p256dh: p256dh.to_string(),
        auth: auth.to_string(),
        label: clean_label(cmd.label.as_deref()),
    })
}

/// The command's endpoint, when it names a known push service.
pub fn validate_endpoint(cmd: &CmdPayload) -> Result<String, RejectReason> {
    let endpoint = cmd.endpoint.as_deref().map(str::trim).unwrap_or("");
    send::vet_endpoint(endpoint)
        .map(|_| endpoint.to_string())
        .ok_or(RejectReason::Malformed)
}

/// A printable, bounded device label.
pub fn clean_label(raw: Option<&str>) -> String {
    raw.unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

pub async fn handle(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = cmd.action.as_str();
    let result = match action {
        "push_config" => config(ctx, cmd).await,
        "push_visibility" => visibility(ctx, cmd),
        "push_subscribe" => subscribe(ctx, cmd).await,
        "push_unsubscribe" => unsubscribe(ctx, cmd).await,
        "push_test" => test(ctx, cmd).await,
        _ => Err(RejectReason::NotAllowed.as_str()),
    };
    match result {
        Ok(()) => {
            accept(ctx, action, None, None).await;
            command_ack(ctx, cmd, true, None).await;
        }
        Err(reason) => {
            let audited = match reason {
                "not_found" => RejectReason::NotFound,
                "action_not_allowed" => RejectReason::NotAllowed,
                _ => RejectReason::Malformed,
            };
            reject(ctx, action, None, audited).await;
            command_ack(ctx, cmd, false, Some(reason)).await;
        }
    }
}

fn hub(ctx: &HandlerCtx) -> Option<super::PushHub> {
    ctx.app.try_state::<crate::remote::RemoteState>().map(|s| s.push.clone())
}

/// Reply with the VAPID key (minted on first use) and this device's state.
async fn send_config(ctx: &HandlerCtx, endpoint: Option<&str>) -> Result<(), &'static str> {
    let key = super::vapid_key().map_err(|e| {
        tracing::warn!(event = "remote_push_vapid_unavailable", error = %e);
        "push_unavailable"
    })?;
    let subscribed = match endpoint.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => store::by_endpoint(&ctx.db, e).await.is_some(),
        None => false,
    };
    send_payload(
        ctx,
        StreamPayload::PushConfig {
            vapid_public_key: super::vapid_public_key(&key),
            subscribed,
            events: store::events(&ctx.db).await,
        },
    )
    .await;
    Ok(())
}

async fn config(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    send_config(ctx, cmd.endpoint.as_deref()).await
}

fn visibility(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    let visible = cmd.visible.ok_or("malformed")?;
    if let Some(h) = hub(ctx) {
        h.set_visible(visible);
    }
    Ok(())
}

async fn subscribe(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    let sub = validate_subscribe(cmd).map_err(RejectReason::as_str)?;
    let now = chrono::Utc::now().to_rfc3339();
    store::upsert(&ctx.db, &sub.endpoint, &sub.p256dh, &sub.auth, &sub.label, &now)
        .await
        .map_err(|e| match e {
            store::UpsertError::TooMany => "too_many_devices",
            store::UpsertError::Db(e) => {
                tracing::warn!(event = "remote_push_subscribe_failed", error = %e);
                "failed"
            }
        })?;
    send_config(ctx, Some(&sub.endpoint)).await
}

async fn unsubscribe(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    // Any endpoint string may be removed (a stale one from before a vetting
    // change included); only the exact match goes.
    let endpoint = cmd.endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()).ok_or("malformed")?;
    store::remove_endpoint(&ctx.db, endpoint).await;
    send_config(ctx, Some(endpoint)).await
}

async fn test(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    let endpoint = validate_endpoint(cmd).map_err(RejectReason::as_str)?;
    let sub = store::by_endpoint(&ctx.db, &endpoint).await.ok_or("not_found")?;
    let hub = hub(ctx).ok_or("push_unavailable")?;
    match super::send_test(&hub, &ctx.db, &sub).await {
        send::Outcome::Delivered => Ok(()),
        send::Outcome::Gone => Err("not_found"),
        send::Outcome::Failed(_) => Err("push_failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P256DH: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    fn cmd(v: serde_json::Value) -> CmdPayload {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_valid_subscription_passes() {
        let c = cmd(serde_json::json!({
            "action": "push_subscribe", "endpoint": " https://web.push.apple.com/QG ",
            "p256dh": P256DH, "auth": AUTH, "label": "iPhone\u{0007} · Safari"
        }));
        assert_eq!(
            validate_subscribe(&c),
            Ok(NewSubscription {
                endpoint: "https://web.push.apple.com/QG".into(),
                p256dh: P256DH.into(),
                auth: AUTH.into(),
                label: "iPhone · Safari".into(),
            })
        );
    }

    #[test]
    fn bad_subscriptions_are_refused() {
        let base = serde_json::json!({
            "action": "push_subscribe", "endpoint": "https://fcm.googleapis.com/x", "p256dh": P256DH, "auth": AUTH
        });
        for (field, value) in [
            ("endpoint", serde_json::json!("https://169.254.169.254/latest")),
            ("endpoint", serde_json::Value::Null),
            ("p256dh", serde_json::json!("BAAA")),
            ("auth", serde_json::json!("short")),
        ] {
            let mut v = base.clone();
            v[field] = value;
            assert_eq!(validate_subscribe(&cmd(v)), Err(RejectReason::Malformed), "{field}");
        }
    }

    #[test]
    fn labels_are_bounded() {
        assert_eq!(clean_label(Some(&"x".repeat(200))).chars().count(), MAX_LABEL_CHARS);
        assert_eq!(clean_label(None), "");
    }
}
