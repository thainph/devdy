//! The Controller's Web Push actions (`push_*`). Reached only through
//! [`crate::remote::handler::handle`], after the allow-list, the rate limit
//! and the `push` feature gate. Every outcome is audited exactly once, except
//! an accepted `push_visibility`: it fires on every app switch and changes
//! nothing but whether pushes are held back.
//!
//! `push_test` waits on a push service (up to the send timeout), so it runs
//! on its own task and is acknowledged when the send finishes — the room's
//! command loop never waits on it.

use super::{send, store};
use crate::remote::command::RejectReason;
use crate::remote::handler::{accept, command_ack, reject, send_payload, HandlerCtx};
use crate::remote::protocol::{CmdPayload, Envelope, FrameType, StreamPayload};
use std::sync::Arc;
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
        "push_events_set" => set_events(ctx, cmd).await,
        "push_test" => match start_test(ctx, cmd).await {
            // Audited and acknowledged by the send task.
            Ok(()) => return,
            Err(reason) => Err(reason),
        },
        _ => Err(RejectReason::NotAllowed.as_str()),
    };
    match result {
        Ok(()) => {
            if action != "push_visibility" {
                accept(ctx, action, None, None).await;
            }
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
    let public = super::vapid_public_key(&key);
    let row = match endpoint.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => store::by_endpoint(&ctx.db, e).await,
        None => None,
    };
    let subscribed = counts_as_subscribed(row.as_ref(), &public);
    send_payload(
        ctx,
        StreamPayload::PushConfig {
            vapid_public_key: public,
            subscribed,
            events: store::events(&ctx.db).await,
            device_events: row.filter(|_| subscribed).map(|r| r.events),
        },
    )
    .await;
    Ok(())
}

/// Whether the Host will notify this row: present, not removed on the
/// desktop, and made for the Host's current key.
pub fn counts_as_subscribed(row: Option<&store::Subscription>, vapid_public_key: &str) -> bool {
    row.is_some_and(|r| !r.removed && r.vapid_key == vapid_public_key)
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
    let key = super::vapid_key().map_err(|_| "push_unavailable")?;
    let vapid_public = super::vapid_public_key(&key);
    let now = chrono::Utc::now().to_rfc3339();
    let row = store::NewRow {
        endpoint: &sub.endpoint,
        p256dh: &sub.p256dh,
        auth: &sub.auth,
        label: &sub.label,
        vapid_key: &vapid_public,
        session_tag: &ctx.push_session_tag,
        automatic: cmd.auto == Some(true),
    };
    store::upsert(&ctx.db, row, &now)
        .await
        .map_err(|e| match e {
            store::UpsertError::TooMany => "too_many_devices",
            store::UpsertError::Removed => "removed_on_desktop",
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

/// Save this device's own per-kind switches, then reply with the fresh config.
async fn set_events(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    let endpoint = validate_endpoint(cmd).map_err(RejectReason::as_str)?;
    let events = cmd.events.ok_or("malformed")?;
    let saved = store::set_device_events(&ctx.db, &endpoint, events).await.map_err(|e| {
        tracing::warn!(event = "remote_push_events_failed", error = %e);
        "failed"
    })?;
    if !saved {
        return Err("not_found");
    }
    send_config(ctx, Some(&endpoint)).await
}

/// The ack reason for a test send's outcome (`None` = delivered).
pub fn test_failure(outcome: &send::Outcome) -> Option<&'static str> {
    match outcome {
        send::Outcome::Delivered => None,
        send::Outcome::Gone => Some("not_found"),
        send::Outcome::Unauthorized | send::Outcome::Failed(_) => Some("push_failed"),
    }
}

/// Validate a `push_test` here, then send it on its own task.
async fn start_test(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<(), &'static str> {
    let endpoint = validate_endpoint(cmd).map_err(RejectReason::as_str)?;
    let sub = store::by_endpoint(&ctx.db, &endpoint).await.filter(|s| !s.removed).ok_or("not_found")?;
    let hub = hub(ctx).ok_or("push_unavailable")?;
    let reply = Reply {
        db: ctx.db.clone(),
        session: ctx.session.clone(),
        room_id: ctx.room_id.clone(),
        out: ctx.out.clone(),
        seq: ctx.seq.clone(),
        command_id: cmd.command_id.clone(),
    };
    tokio::spawn(async move {
        let failure = test_failure(&super::send_test(&hub, &reply.db, &sub).await);
        reply.finish("push_test", failure).await;
    });
    Ok(())
}

/// What a detached task needs to answer the command that started it. Holds
/// the room it came from: if that room is gone by then, the frame goes nowhere.
struct Reply {
    db: crate::db::Db,
    session: Arc<remote_e2e::Session>,
    room_id: String,
    out: crate::remote::outbound::OutboundTx,
    seq: Arc<crate::remote::forwarder::SeqCounter>,
    command_id: Option<String>,
}

impl Reply {
    /// Audit the outcome and send the correlated `command_ack`.
    async fn finish(&self, action: &str, failure: Option<&'static str>) {
        let (result, audited) = match failure {
            None => (crate::remote::audit::RESULT_ACCEPTED, None),
            Some("not_found") => (crate::remote::audit::RESULT_REJECTED, Some(RejectReason::NotFound.as_str())),
            Some(_) => (crate::remote::audit::RESULT_REJECTED, Some(RejectReason::Malformed.as_str())),
        };
        crate::remote::audit::audit(&self.db, None, Some(&self.room_id), None, action, result, audited).await;
        let Some(command_id) = self.command_id.clone() else { return };
        let payload = StreamPayload::CommandAck {
            command_id,
            action: action.to_string(),
            accepted: failure.is_none(),
            reason: failure.map(str::to_string),
            run_id: None,
            item_id: None,
        };
        if let Ok(json) = serde_json::to_vec(&payload) {
            let cipher = self.session.seal(&json);
            let _ = self.out.send(Envelope::data(FrameType::Stream, &self.room_id, cipher, self.seq.next()));
        }
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
    fn test_send_outcomes_map_to_ack_reasons() {
        assert_eq!(test_failure(&send::Outcome::Delivered), None);
        assert_eq!(test_failure(&send::Outcome::Gone), Some("not_found"));
        assert_eq!(test_failure(&send::Outcome::Failed("http 500".into())), Some("push_failed"));
    }

    #[test]
    fn only_a_live_row_for_the_current_key_counts_as_subscribed() {
        let row = |removed: bool, key: &str| store::Subscription {
            id: "i".into(),
            endpoint: "e".into(),
            p256dh: "p".into(),
            auth: "a".into(),
            label: String::new(),
            events: Default::default(),
            created_at: "t".into(),
            last_ok_at: None,
            vapid_key: key.into(),
            session_tag: String::new(),
            removed,
            auth_failures: 0,
        };
        assert!(counts_as_subscribed(Some(&row(false, "K")), "K"));
        assert!(!counts_as_subscribed(Some(&row(false, "OLD")), "K"), "made for another key");
        assert!(!counts_as_subscribed(Some(&row(true, "K")), "K"), "removed on the desktop");
        assert!(!counts_as_subscribed(None, "K"));
    }

    #[test]
    fn labels_are_bounded() {
        assert_eq!(clean_label(Some(&"x".repeat(200))).chars().count(), MAX_LABEL_CHARS);
        assert_eq!(clean_label(None), "");
    }
}
