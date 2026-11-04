//! Delivering one push: endpoint vetting, the HTTP request to the browser's
//! push service, and what its answer means for the stored subscription.

use super::coalesce::PushMessage;
use super::crypto;
use super::store::{self, Subscription};
use crate::db::Db;
use p256::SecretKey;
use reqwest::Url;

/// Push services a subscription may point at. The endpoint comes from the
/// phone; without this list a paired device could make the Host POST to any
/// URL, internal network included.
const PUSH_HOSTS: [&str; 5] = [
    "fcm.googleapis.com",
    "android.googleapis.com",
    "push.services.mozilla.com",
    "push.apple.com",
    "notify.windows.com",
];

/// Longest endpoint accepted (real ones are a few hundred bytes).
pub const MAX_ENDPOINT_LEN: usize = 2048;

/// How long an undelivered notification stays queued at the push service.
const TTL_SECS: u32 = 3600;

/// The endpoint parsed, when it is an `https` URL on a known push service.
pub fn vet_endpoint(endpoint: &str) -> Option<Url> {
    if endpoint.len() > MAX_ENDPOINT_LEN {
        return None;
    }
    let url = Url::parse(endpoint).ok()?;
    if url.scheme() != "https" || url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let known = PUSH_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")));
    known.then_some(url)
}

/// The VAPID `sub` claim: the configured contact, else the relay's https
/// origin (a URL its operator controls), else a placeholder mailbox.
pub fn vapid_subject(contact: Option<&str>, relay_url: Option<&str>) -> String {
    if let Some(c) = contact.map(str::trim).filter(|c| c.starts_with("mailto:") || c.starts_with("https://")) {
        return c.to_string();
    }
    if let Some(url) = relay_url.and_then(|u| Url::parse(u.trim()).ok()) {
        if let Some(host) = url.host_str() {
            if matches!(url.scheme(), "wss" | "https") {
                return format!("https://{host}");
            }
        }
    }
    "mailto:devdy-remote@users.noreply.github.com".to_string()
}

/// What the push service said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// 404 / 410: the subscription expired or was revoked — forget it.
    Gone,
    /// 401 / 403: the push service refused our VAPID credentials for this
    /// subscription. Once could be a clock blip; several in a row means gone.
    Unauthorized,
    Failed(String),
}

pub fn classify(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Delivered,
        404 | 410 => Outcome::Gone,
        401 | 403 => Outcome::Unauthorized,
        s => Outcome::Failed(format!("http {s}")),
    }
}

/// POST one encrypted message to a subscription.
pub async fn send(client: &reqwest::Client, vapid: &SecretKey, subject: &str, sub: &Subscription, msg: &PushMessage) -> Outcome {
    let Some(url) = vet_endpoint(&sub.endpoint) else {
        return Outcome::Gone;
    };
    let (Some(ua_public), Some(auth)) = (crypto::parse_public_key(&sub.p256dh), crypto::parse_auth_secret(&sub.auth)) else {
        return Outcome::Gone;
    };
    let plaintext = match serde_json::to_vec(msg) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let body = match crypto::encrypt(&ua_public, &auth, &plaintext) {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(e),
    };
    let audience = url.origin().ascii_serialization();
    let authorization = crypto::vapid_authorization(vapid, &audience, subject, crate::remote::agent::now_unix());
    let urgency = urgency(msg);
    // A queued reminder must not be replaced by the next push.
    let send_topic = sends_topic(&url) && !msg.is_focus();
    let mut req = client
        .post(url)
        .header("TTL", TTL_SECS.to_string())
        .header("Urgency", urgency);
    if send_topic {
        // A queued, undelivered notification is replaced by the next one.
        req = req.header("Topic", "devdy");
    }
    let res = req
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("Authorization", authorization)
        .body(body)
        .send()
        .await;
    match res {
        Ok(r) => {
            let status = r.status().as_u16();
            let outcome = classify(status);
            if outcome == Outcome::Delivered {
                return outcome;
            }
            // Apple answers `{"reason":"VapidPkHashMismatch"}` and the like;
            // without it a refusal is just a status code.
            let reason = service_reason(r).await;
            tracing::warn!(event = "remote_push_refused", host = ?url_host(&sub.endpoint), status, reason = %reason);
            match outcome {
                Outcome::Failed(s) if !reason.is_empty() => Outcome::Failed(format!("{s} {reason}")),
                other => other,
            }
        }
        Err(e) => Outcome::Failed(if e.is_timeout() { "timeout".into() } else { "network".into() }),
    }
}

/// Prompts, reminders and the test are time-sensitive; finished runs are not.
fn urgency(msg: &PushMessage) -> &'static str {
    if msg.pending.unwrap_or(0) > 0 || msg.t == "test" || msg.is_focus() {
        "high"
    } else {
        "normal"
    }
}

/// Whether to send the RFC 8030 `Topic` header. Apple's push service refuses
/// it (`400 BadWebPushTopic`), so iPhones get every queued notification
/// instead of only the latest.
fn sends_topic(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    !(host == "push.apple.com" || host.ends_with(".push.apple.com"))
}

/// Longest refusal body read from a push service.
const MAX_REASON_BODY: usize = 1024;

/// Why a push service refused: the `reason` of a JSON body (Apple, Mozilla),
/// else the start of a text one (Google) — printable characters only.
async fn service_reason(r: reqwest::Response) -> String {
    let body = r.bytes().await.unwrap_or_default();
    reason_from_body(&body[..body.len().min(MAX_REASON_BODY)])
}

pub fn reason_from_body(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let raw = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_else(|| text.into_owned());
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The endpoint's host only: its path is a bearer capability.
fn url_host(endpoint: &str) -> Option<String> {
    Url::parse(endpoint).ok().and_then(|u| u.host_str().map(str::to_string))
}

/// Record what a delivery attempt means for the stored subscription.
pub async fn apply_outcome(db: &Db, sub: &Subscription, outcome: &Outcome) {
    match outcome {
        Outcome::Unauthorized => {
            if store::note_auth_failure(db, &sub.id).await {
                store::remove_id(db, &sub.id).await;
                tracing::info!(event = "remote_push_subscription_unauthorized", id = %sub.id);
            }
        }
        Outcome::Delivered => store::mark_ok(db, &sub.id, &chrono::Utc::now().to_rfc3339()).await,
        Outcome::Gone => {
            store::remove_id(db, &sub.id).await;
            tracing::info!(event = "remote_push_subscription_gone", id = %sub.id);
        }
        Outcome::Failed(reason) => {
            tracing::warn!(event = "remote_push_failed", id = %sub.id, host = ?url_host(&sub.endpoint), reason = %reason);
        }
    }
}

/// [`send`] + [`apply_outcome`].
pub async fn deliver(db: &Db, client: &reqwest::Client, vapid: &SecretKey, subject: &str, sub: &Subscription, msg: &PushMessage) -> Outcome {
    let outcome = send(client, vapid, subject, sub, msg).await;
    apply_outcome(db, sub, &outcome).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::push::store::tests::{mem_db, row};

    #[test]
    fn only_https_push_services_are_accepted() {
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            "https://web.push.apple.com/QGx",
            "https://wns2-db5p.notify.windows.com/w/?token=x",
        ] {
            assert!(vet_endpoint(ok).is_some(), "{ok}");
        }
        for bad in [
            "http://fcm.googleapis.com/fcm/send/abc",
            "https://fcm.googleapis.com:8443/x",
            "https://evilfcm.googleapis.com.attacker.net/x",
            "https://notpush.apple.com.evil/x",
            "https://127.0.0.1/x",
            "https://localhost/x",
            "https://user:pw@fcm.googleapis.com/x",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(vet_endpoint(bad).is_none(), "{bad}");
        }
        let long = format!("https://fcm.googleapis.com/{}", "a".repeat(MAX_ENDPOINT_LEN));
        assert!(vet_endpoint(&long).is_none());
    }

    #[test]
    fn subject_prefers_contact_then_relay_origin() {
        assert_eq!(vapid_subject(Some("mailto:me@x.io"), Some("wss://r.x.io/ws")), "mailto:me@x.io");
        assert_eq!(vapid_subject(Some("bogus"), Some("wss://r.x.io/ws")), "https://r.x.io");
        assert_eq!(vapid_subject(None, Some("ws://10.0.0.2:8080/ws")), "mailto:devdy-remote@users.noreply.github.com");
        assert_eq!(vapid_subject(None, None), "mailto:devdy-remote@users.noreply.github.com");
    }

    #[test]
    fn status_codes_classify() {
        assert_eq!(classify(201), Outcome::Delivered);
        assert_eq!(classify(404), Outcome::Gone);
        assert_eq!(classify(410), Outcome::Gone);
        assert_eq!(classify(401), Outcome::Unauthorized);
        assert_eq!(classify(403), Outcome::Unauthorized);
        assert_eq!(classify(429), Outcome::Failed("http 429".into()));
        assert_eq!(classify(413), Outcome::Failed("http 413".into()));
    }

    #[test]
    fn apple_gets_no_topic_header() {
        let topic = |u: &str| sends_topic(&Url::parse(u).unwrap());
        assert!(!topic("https://web.push.apple.com/QGx"));
        assert!(topic("https://fcm.googleapis.com/fcm/send/abc"));
        assert!(topic("https://updates.push.services.mozilla.com/wpush/v2/x"));
    }

    #[test]
    fn reminders_go_out_urgent() {
        assert_eq!(urgency(&PushMessage::focus("Focus", "Reminder: A", None)), "high");
        assert_eq!(urgency(&PushMessage::test()), "high");
        let done = super::super::coalesce::compose(
            super::super::coalesce::BurstCounts { new_pending: 0, done: 1, failed: 0 },
            0,
            Default::default(),
        )
        .unwrap();
        assert_eq!(urgency(&done), "normal");
    }

    #[test]
    fn refusal_reasons_are_short_and_printable() {
        assert_eq!(reason_from_body(br#"{"reason":"VapidPkHashMismatch"}"#), "VapidPkHashMismatch");
        assert_eq!(reason_from_body(b"invalid JWT provided\n"), "invalid JWT provided");
        assert_eq!(reason_from_body(b""), "");
        assert_eq!(reason_from_body("x".repeat(500).as_bytes()).len(), 120);
    }

    #[tokio::test]
    async fn a_gone_subscription_is_removed_and_a_delivery_is_recorded() {
        let db = mem_db().await;
        store::upsert(&db, row("https://fcm.googleapis.com/a"), "t").await.unwrap();
        store::upsert(&db, row("https://fcm.googleapis.com/b"), "t").await.unwrap();
        let a = store::by_endpoint(&db, "https://fcm.googleapis.com/a").await.unwrap();
        let b = store::by_endpoint(&db, "https://fcm.googleapis.com/b").await.unwrap();

        apply_outcome(&db, &a, &classify(410)).await;
        assert!(store::by_id(&db, &a.id).await.is_none());

        apply_outcome(&db, &b, &classify(500)).await;
        let kept = store::by_id(&db, &b.id).await.unwrap();
        assert_eq!(kept.last_ok_at, None, "a failure keeps the device, unmarked");

        apply_outcome(&db, &b, &classify(201)).await;
        assert!(store::by_id(&db, &b.id).await.unwrap().last_ok_at.is_some());
    }

    #[tokio::test]
    async fn repeated_auth_refusals_count_as_gone_and_a_delivery_resets_them() {
        let db = mem_db().await;
        store::upsert(&db, row("https://web.push.apple.com/a"), "t").await.unwrap();
        let a = store::by_endpoint(&db, "https://web.push.apple.com/a").await.unwrap();
        apply_outcome(&db, &a, &classify(403)).await;
        apply_outcome(&db, &a, &classify(401)).await;
        apply_outcome(&db, &a, &classify(201)).await;
        assert_eq!(store::by_id(&db, &a.id).await.unwrap().auth_failures, 0);
        for _ in 0..store::MAX_AUTH_FAILURES - 1 {
            apply_outcome(&db, &a, &classify(403)).await;
        }
        assert!(store::by_id(&db, &a.id).await.is_some(), "not yet");
        apply_outcome(&db, &a, &classify(403)).await;
        assert!(store::by_id(&db, &a.id).await.is_none());
    }
}
