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
    Failed(String),
}

pub fn classify(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Delivered,
        404 | 410 => Outcome::Gone,
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
    let urgency = if msg.pending.unwrap_or(0) > 0 || msg.t == "test" { "high" } else { "normal" };
    let res = client
        .post(url)
        .header("TTL", TTL_SECS.to_string())
        .header("Urgency", urgency)
        // A queued, undelivered notification is replaced by the next one.
        .header("Topic", "devdy")
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("Authorization", authorization)
        .body(body)
        .send()
        .await;
    match res {
        Ok(r) => classify(r.status().as_u16()),
        Err(e) => Outcome::Failed(if e.is_timeout() { "timeout".into() } else { "network".into() }),
    }
}

/// Record what a delivery attempt means for the stored subscription.
pub async fn apply_outcome(db: &Db, sub: &Subscription, outcome: &Outcome) {
    match outcome {
        Outcome::Delivered => store::mark_ok(db, &sub.id, &chrono::Utc::now().to_rfc3339()).await,
        Outcome::Gone => {
            store::remove_id(db, &sub.id).await;
            tracing::info!(event = "remote_push_subscription_gone", id = %sub.id);
        }
        Outcome::Failed(reason) => {
            // Host only: the endpoint path is a bearer capability.
            let host = Url::parse(&sub.endpoint).ok().and_then(|u| u.host_str().map(str::to_string));
            tracing::warn!(event = "remote_push_failed", id = %sub.id, host = ?host, reason = %reason);
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
    use crate::remote::push::store::tests::mem_db;

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
        assert_eq!(classify(429), Outcome::Failed("http 429".into()));
        assert_eq!(classify(413), Outcome::Failed("http 413".into()));
    }

    #[tokio::test]
    async fn a_gone_subscription_is_removed_and_a_delivery_is_recorded() {
        let db = mem_db().await;
        store::upsert(&db, "https://fcm.googleapis.com/a", "k", "a", "A", "t").await.unwrap();
        store::upsert(&db, "https://fcm.googleapis.com/b", "k", "a", "B", "t").await.unwrap();
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
}
