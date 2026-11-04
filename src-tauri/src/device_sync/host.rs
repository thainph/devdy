//! The serving side of device sync (device A), driven by the Remote Control
//! agent: it shares the agent's relay socket but lives in its own listen slot
//! and room, so a pull never touches the phone / remote-desktop session.
//!
//! See `transport.rs` for the protocol. Served only while the agent runs
//! (Remote Control on) and a master password is set; wrong passwords count
//! against the password-login budget (`remote::login::FAILURES`), and every
//! pull attempt lands in the Remote Control audit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use remote_e2e::{host_fingerprint, Keypair, Session};
use tauri::AppHandle;

use crate::db::Db;
use crate::device_sync::bundle::{build_sync_bundle, SyncCategories};
use crate::device_sync::transport::{
    bundle_key, chunk_count, control_frame, open_msg, random_bytes, seal_msg, sha256_hex,
    sync_code, SyncMsg, CHUNK_BYTES, ERR_BAD_REQUEST, ERR_RATE_LIMITED, ERR_SOURCE_FAILED,
    ERR_TOO_LARGE, ERR_WRONG_PASSWORD, MAX_BUNDLE_BYTES, PROTOCOL_VERSION, RANDOM_LEN, SYNC_SLOT,
};
use crate::remote::audit;
use crate::remote::forwarder::SeqCounter;
use crate::remote::login::FailureLimiter;
use crate::remote::outbound::OutboundTx;
use crate::remote::protocol::{Envelope, FrameType, HandshakePayload};
use crate::remote::{pairing_hex, pairing_unhex};

/// Audit action of a pull.
pub const AUDIT_ACTION: &str = "device_sync_pull";
/// Outbound depth above which the serve task waits before the next chunk, so a
/// pull never buries the live Remote Control traffic sharing the socket.
const SERVE_HIGH_WATER: usize = 16;

/// A pull in progress on the sync slot.
pub struct SyncPending {
    pub room_id: String,
    keypair: Keypair,
    puller_pub: Option<Vec<u8>>,
    /// Set once authorized: builds and streams the bundle.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for SyncPending {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

/// The code to listen on: this device's sync code while serving is allowed.
pub fn listen_code(device_id: &str, serving: bool) -> Option<String> {
    serving.then(|| sync_code(device_id))
}

/// The frame that moves the sync slot from `current` to `want`, if any.
pub fn listen_frame(current: Option<&str>, want: Option<&str>) -> Option<Envelope> {
    if current == want {
        return None;
    }
    Some(match want {
        Some(code) => Envelope::create_pair_slot(code, SYNC_SLOT),
        None => Envelope::revoke_slot(SYNC_SLOT),
    })
}

/// Whether `env` belongs to the sync flow (and must stay away from the
/// session and login paths).
pub fn is_sync_frame(env: &Envelope, pending: Option<&SyncPending>) -> bool {
    if env.t == FrameType::RoomReady {
        return env.slot.as_deref() == Some(SYNC_SLOT);
    }
    pending.is_some_and(|p| !env.room_id.is_empty() && p.room_id == env.room_id)
}

/// A refused pull, by the code the puller sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    RateLimited,
    WrongPassword,
    BadRequest,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::RateLimited => ERR_RATE_LIMITED,
            Refusal::WrongPassword => ERR_WRONG_PASSWORD,
            Refusal::BadRequest => ERR_BAD_REQUEST,
        }
    }
}

/// An authorized pull request.
pub struct Authorized {
    pub session: Session,
    pub categories: SyncCategories,
    pub nonce: Vec<u8>,
}

/// Gate the puller's first sealed frame: refused outright while the failure
/// budget is spent; a frame that does not open under `psk` is a wrong
/// password and spends one failure.
pub fn authorize(
    limiter: &mut FailureLimiter,
    now: Instant,
    psk: &[u8; 32],
    keypair: &Keypair,
    puller_pub: &[u8],
    cipher: &str,
) -> Result<Authorized, Refusal> {
    if !limiter.allow(now) {
        return Err(Refusal::RateLimited);
    }
    let Some(session) = Session::new(psk, keypair, puller_pub).ok() else {
        return Err(Refusal::BadRequest);
    };
    if session.open(cipher).is_err() {
        limiter.record_failure(now);
        return Err(Refusal::WrongPassword);
    }
    // It opened: the password is right; what remains is the request itself.
    match open_msg(&session, cipher) {
        Some(SyncMsg::Request {
            v,
            categories,
            nonce,
        }) if v == PROTOCOL_VERSION => {
            let nonce = pairing_unhex(&nonce)
                .filter(|n| n.len() == RANDOM_LEN)
                .ok_or(Refusal::BadRequest)?;
            Ok(Authorized {
                session,
                categories,
                nonce,
            })
        }
        _ => Err(Refusal::BadRequest),
    }
}

/// The `hs_offer` for a new pull room, and its pending state.
pub fn offer(room_id: &str, seq: u64) -> Option<(SyncPending, Envelope)> {
    let keypair = Keypair::generate();
    let offer = HandshakePayload::HsOffer {
        host_pub: pairing_hex(&keypair.public),
        host_fingerprint: host_fingerprint(&keypair.public).ok()?,
    };
    let frame = control_frame(room_id, seq, &offer)?;
    Some((
        SyncPending {
            room_id: room_id.to_string(),
            keypair,
            puller_pub: None,
            task: None,
        },
        frame,
    ))
}

/// The sealed frames that carry `bundle`: a `start` header, then the chunks.
/// Lazy, so only one sealed chunk is ever held at a time.
pub fn bundle_frames<'a>(
    session: &'a Session,
    room_id: &'a str,
    seq: &'a SeqCounter,
    salt: &[u8],
    bundle: &'a [u8],
) -> impl Iterator<Item = Option<Envelope>> + 'a {
    let start = SyncMsg::Start {
        v: PROTOCOL_VERSION,
        salt: pairing_hex(salt),
        total: bundle.len() as u64,
        chunks: chunk_count(bundle.len() as u64),
        sha256: sha256_hex(bundle),
    };
    std::iter::once(seal_msg(
        session,
        FrameType::Stream,
        room_id,
        seq.next(),
        &start,
    ))
    .chain(bundle.chunks(CHUNK_BYTES).enumerate().map(move |(i, c)| {
        let chunk = SyncMsg::Chunk {
            index: i as u32,
            data: B64.encode(c),
        };
        seal_msg(session, FrameType::Stream, room_id, seq.next(), &chunk)
    }))
}

/// Drive one sync-room frame: offer on join, keep the puller's key from the
/// answer, gate + serve on the first sealed frame.
pub async fn handle_frame(
    env: Envelope,
    app: &AppHandle,
    db: &Db,
    out_tx: &OutboundTx,
    seq: &Arc<SeqCounter>,
    pending: &mut Option<SyncPending>,
) {
    match env.t {
        FrameType::RoomReady => {
            if env.code.as_deref() == Some("armed") || env.room_id.is_empty() {
                return;
            }
            // A newer join on the code replaced the previous puller's room.
            *pending = None;
            if let Some((p, frame)) = offer(&env.room_id, seq.next()) {
                let _ = out_tx.send(frame);
                tracing::info!(event = "device_sync_offered", room_id = %env.room_id);
                *pending = Some(p);
            }
        }
        FrameType::Control => {
            let Some(p) = pending.as_mut() else { return };
            if p.task.is_some() {
                return;
            }
            if let Some(HandshakePayload::HsAnswer { controller_pub }) = env
                .cipher
                .as_deref()
                .and_then(crate::device_sync::transport::parse_control)
            {
                p.puller_pub = pairing_unhex(&controller_pub).filter(|b| b.len() == 32);
            }
        }
        FrameType::Cmd => {
            let Some(p) = pending.as_mut() else { return };
            if p.task.is_some() {
                return;
            }
            let (Some(puller_pub), Some(cipher)) = (p.puller_pub.clone(), env.cipher) else {
                return;
            };
            let room_id = p.room_id.clone();
            let Some(password) = crate::secrets::get_remote_master_password() else {
                refuse(
                    db,
                    out_tx,
                    seq,
                    pending,
                    Refusal::BadRequest,
                    "no_master_password",
                )
                .await;
                return;
            };
            let Ok(psk) =
                tokio::task::spawn_blocking(move || crate::remote::login::login_psk(&password))
                    .await
            else {
                return;
            };
            let verdict = match crate::remote::login::FAILURES.lock() {
                Ok(mut limiter) => authorize(
                    &mut limiter,
                    Instant::now(),
                    &psk,
                    &p.keypair,
                    &puller_pub,
                    &cipher,
                ),
                Err(_) => Err(Refusal::RateLimited),
            };
            let auth = match verdict {
                Ok(a) => a,
                Err(r) => {
                    let reason = match r {
                        Refusal::RateLimited => "throttled",
                        Refusal::WrongPassword => "bad_password",
                        Refusal::BadRequest => "bad_request",
                    };
                    tracing::warn!(event = "device_sync_refused", room_id = %room_id, reason);
                    refuse(db, out_tx, seq, pending, r, reason).await;
                    return;
                }
            };
            let what = category_list(&auth.categories);
            audit::audit(
                db,
                None,
                Some(&room_id),
                None,
                AUDIT_ACTION,
                audit::RESULT_ACCEPTED,
                Some(&what),
            )
            .await;
            tracing::info!(event = "device_sync_authorized", room_id = %room_id, categories = %what);
            let host_pub = p.keypair.public;
            let task = tokio::spawn(serve(
                app.clone(),
                db.clone(),
                auth,
                psk,
                host_pub,
                puller_pub,
                room_id,
                out_tx.clone(),
                seq.clone(),
            ));
            p.task = Some(task);
        }
        // The puller left (or the room is gone): stop serving.
        FrameType::PeerLeft | FrameType::Error => {
            *pending = None;
        }
        _ => {}
    }
}

/// Tell the puller why, close its room, audit the refusal.
async fn refuse(
    db: &Db,
    out_tx: &OutboundTx,
    seq: &Arc<SeqCounter>,
    pending: &mut Option<SyncPending>,
    refusal: Refusal,
    audit_reason: &str,
) {
    let Some(p) = pending.take() else { return };
    let reject = HandshakePayload::SyncReject {
        reason: refusal.code().to_string(),
    };
    if let Some(frame) = control_frame(&p.room_id, seq.next(), &reject) {
        let _ = out_tx.send(frame);
    }
    let _ = out_tx.send(Envelope::revoke(p.room_id.clone()));
    audit::audit(
        db,
        None,
        Some(&p.room_id),
        None,
        AUDIT_ACTION,
        audit::RESULT_REJECTED,
        Some(audit_reason),
    )
    .await;
}

/// Build the bundle under the pull's key and stream it, pacing on the shared
/// outbound queue.
#[allow(clippy::too_many_arguments)]
async fn serve(
    app: AppHandle,
    db: Db,
    auth: Authorized,
    psk: [u8; 32],
    host_pub: [u8; 32],
    puller_pub: Vec<u8>,
    room_id: String,
    out_tx: OutboundTx,
    seq: Arc<SeqCounter>,
) {
    let salt = random_bytes();
    let built = match bundle_key(&psk, &salt, &auth.nonce, &host_pub, &puller_pub) {
        Ok(key) => build_sync_bundle(&app, &db, &auth.categories, &key).await,
        Err(e) => Err(e),
    };
    let failure = match &built {
        Ok(b) if b.len() as u64 > MAX_BUNDLE_BYTES => Some(ERR_TOO_LARGE),
        Ok(_) => None,
        Err(e) if e == ERR_TOO_LARGE => Some(ERR_TOO_LARGE),
        Err(_) => Some(ERR_SOURCE_FAILED),
    };
    if let Some(reason) = failure {
        // The bundle error may name local data; it stays out of logs and audit.
        tracing::warn!(event = "device_sync_build_failed", room_id = %room_id, reason);
        let msg = SyncMsg::Failed {
            reason: reason.to_string(),
        };
        if let Some(frame) = seal_msg(&auth.session, FrameType::Stream, &room_id, seq.next(), &msg)
        {
            let _ = out_tx.send(frame);
        }
        audit::audit(
            &db,
            None,
            Some(&room_id),
            None,
            AUDIT_ACTION,
            audit::RESULT_REJECTED,
            Some(reason),
        )
        .await;
        return;
    }
    let Ok(bundle) = built else { return };
    for frame in bundle_frames(&auth.session, &room_id, &seq, &salt, &bundle) {
        while out_tx.depth() > SERVE_HIGH_WATER {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let Some(frame) = frame else { return };
        if out_tx.send(frame).is_err() {
            return;
        }
    }
    tracing::info!(event = "device_sync_served", room_id = %room_id, bytes = bundle.len());
}

/// The requested categories as a short list for the audit (names only).
pub fn category_list(c: &SyncCategories) -> String {
    let all = [
        ("todos", c.todos),
        ("notes", c.notes),
        ("secret_notes", c.secret_notes),
        ("slack_threads", c.slack_threads),
        ("skills_rules", c.skills_rules),
        ("projects", c.projects),
        ("settings", c.settings),
        ("mcp_servers", c.mcp_servers),
        ("vps_servers", c.vps_servers),
        ("accounts", c.accounts),
    ];
    let on: Vec<&str> = all.iter().filter(|(_, v)| *v).map(|(k, _)| *k).collect();
    if on.is_empty() {
        "none".to_string()
    } else {
        on.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_sync::transport::{Assembler, ERR_CHECKSUM};
    use crate::remote::login::MAX_FAILURES;

    /// A puller's first frame sealed under `psk`.
    fn request(psk: &[u8; 32], host: &Keypair) -> (Keypair, String) {
        let puller = Keypair::generate();
        let s = Session::new(psk, &puller, &host.public).unwrap();
        let req = SyncMsg::Request {
            v: PROTOCOL_VERSION,
            categories: SyncCategories {
                todos: true,
                ..Default::default()
            },
            nonce: pairing_hex(&random_bytes()),
        };
        (puller, s.seal(&serde_json::to_vec(&req).unwrap()))
    }

    #[test]
    fn the_right_password_is_authorized() {
        let psk = [1u8; 32];
        let host = Keypair::generate();
        let (puller, cipher) = request(&psk, &host);
        let mut limiter = FailureLimiter::new();
        let a = authorize(
            &mut limiter,
            Instant::now(),
            &psk,
            &host,
            &puller.public,
            &cipher,
        )
        .ok()
        .unwrap();
        assert!(a.categories.todos && !a.categories.notes);
        assert_eq!(a.nonce.len(), RANDOM_LEN);
    }

    #[test]
    fn a_wrong_password_is_refused_and_rate_limited() {
        let host_psk = [1u8; 32];
        let host = Keypair::generate();
        let mut limiter = FailureLimiter::new();
        let now = Instant::now();
        for _ in 0..MAX_FAILURES {
            let (puller, cipher) = request(&[2u8; 32], &host);
            let r = authorize(&mut limiter, now, &host_psk, &host, &puller.public, &cipher);
            assert_eq!(r.err(), Some(Refusal::WrongPassword));
        }
        // The budget is spent: even the right password is refused, untried.
        let (puller, cipher) = request(&host_psk, &host);
        let r = authorize(&mut limiter, now, &host_psk, &host, &puller.public, &cipher);
        assert_eq!(r.err(), Some(Refusal::RateLimited));
        assert_eq!(Refusal::RateLimited.code(), "rate_limited");
        assert_eq!(Refusal::WrongPassword.code(), "wrong_password");
        // And it recovers after the window.
        let later = now + crate::remote::login::FAILURE_WINDOW;
        assert!(authorize(
            &mut limiter,
            later,
            &host_psk,
            &host,
            &puller.public,
            &cipher
        )
        .is_ok());
    }

    #[test]
    fn a_frame_sealed_for_another_room_is_a_wrong_password() {
        let psk = [1u8; 32];
        let host = Keypair::generate();
        let other_host = Keypair::generate();
        let (puller, cipher) = request(&psk, &other_host);
        let mut limiter = FailureLimiter::new();
        let r = authorize(
            &mut limiter,
            Instant::now(),
            &psk,
            &host,
            &puller.public,
            &cipher,
        );
        assert_eq!(r.err(), Some(Refusal::WrongPassword));
    }

    #[test]
    fn served_frames_reassemble_on_the_puller() {
        let psk = [3u8; 32];
        let host = Keypair::generate();
        let puller = Keypair::generate();
        let host_s = Session::new(&psk, &host, &puller.public).unwrap();
        let puller_s = Session::new(&psk, &puller, &host.public).unwrap();
        let bundle: Vec<u8> = (0..(2 * CHUNK_BYTES + 99))
            .map(|i| (i % 253) as u8)
            .collect();
        let seq = SeqCounter::default();
        let salt = random_bytes();
        let frames: Vec<Envelope> = bundle_frames(&host_s, "rm_s", &seq, &salt, &bundle)
            .map(Option::unwrap)
            .collect();
        assert_eq!(frames.len(), 4, "start + 3 chunks");

        let mut assembler = None;
        for f in &frames {
            assert_eq!(f.t, FrameType::Stream);
            assert_eq!(f.room_id, "rm_s");
            match open_msg(&puller_s, f.cipher.as_deref().unwrap()).unwrap() {
                SyncMsg::Start {
                    total,
                    chunks,
                    sha256,
                    salt: s,
                    ..
                } => {
                    assert_eq!(pairing_unhex(&s).unwrap(), salt);
                    assembler = Some(Assembler::new(total, chunks, &sha256).unwrap());
                }
                SyncMsg::Chunk { index, data } => {
                    assembler
                        .as_mut()
                        .unwrap()
                        .push(index, &B64.decode(data).unwrap())
                        .unwrap();
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(assembler.unwrap().finish().unwrap(), bundle);

        // A tampered header (wrong sha) is caught on the puller.
        let mut a = Assembler::new(bundle.len() as u64, 3, &sha256_hex(b"x")).unwrap();
        for (i, c) in bundle.chunks(CHUNK_BYTES).enumerate() {
            a.push(i as u32, c).unwrap();
        }
        assert_eq!(a.finish().unwrap_err(), ERR_CHECKSUM);
    }

    #[test]
    fn sync_frames_never_reach_the_session_or_login_paths() {
        let mut ready = Envelope::data(FrameType::Control, "rm_x", String::new(), 0);
        ready.t = FrameType::RoomReady;
        assert!(!is_sync_frame(&ready, None), "session join");
        ready.slot = Some("login".into());
        assert!(!is_sync_frame(&ready, None), "login join");
        ready.slot = Some(SYNC_SLOT.into());
        assert!(is_sync_frame(&ready, None), "sync join");

        let (p, _) = offer("rm_sync", 1).unwrap();
        let mine = Envelope::data(FrameType::Cmd, "rm_sync", "c".into(), 2);
        let session = Envelope::data(FrameType::Cmd, "rm_session", "c".into(), 3);
        assert!(is_sync_frame(&mine, Some(&p)));
        assert!(
            !is_sync_frame(&session, Some(&p)),
            "the phone's frames stay on the session path"
        );
        assert!(!is_sync_frame(&session, None));
        let mut left = Envelope::revoke("rm_session");
        left.t = FrameType::PeerLeft;
        assert!(
            !is_sync_frame(&left, Some(&p)),
            "the phone leaving is not the pull ending"
        );
    }

    #[test]
    fn listen_follows_serving() {
        assert_eq!(listen_code("dev-a", false), None);
        assert_eq!(
            listen_code("dev-a", true).as_deref(),
            Some("devdy-sync-v1/dev-a")
        );
        assert!(listen_frame(None, None).is_none());
        let arm = listen_frame(None, Some("devdy-sync-v1/dev-a")).unwrap();
        assert_eq!(arm.t, FrameType::CreatePair);
        assert_eq!(arm.slot.as_deref(), Some(SYNC_SLOT));
        assert!(listen_frame(Some("devdy-sync-v1/dev-a"), Some("devdy-sync-v1/dev-a")).is_none());
        let off = listen_frame(Some("devdy-sync-v1/dev-a"), None).unwrap();
        assert_eq!(off.t, FrameType::Revoke);
        assert_eq!(off.slot.as_deref(), Some(SYNC_SLOT));
    }

    #[test]
    fn audit_lists_category_names_only() {
        let c = SyncCategories {
            todos: true,
            accounts: true,
            ..Default::default()
        };
        assert_eq!(category_list(&c), "todos,accounts");
        assert_eq!(category_list(&SyncCategories::default()), "none");
    }

    /// How the fake relay + host behaves.
    #[derive(Clone, Copy)]
    enum Mode {
        Serve,
        TamperSha,
        Offline,
    }

    type TestWs = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn ws_send(ws: &mut TestWs, e: Envelope) {
        use futures_util::SinkExt;
        let text = serde_json::to_string(&e).unwrap();
        ws.send(tokio_tungstenite::tungstenite::Message::Text(text))
            .await
            .unwrap();
    }

    async fn ws_next(ws: &mut TestWs) -> Option<Envelope> {
        use futures_util::StreamExt;
        loop {
            match ws.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => {
                    return serde_json::from_str(&t).ok()
                }
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    }

    /// One-connection stand-in for the relay with host A behind it, built on
    /// the real host helpers. Returns the URL and the key A sealed with.
    async fn fake_host(
        mode: Mode,
        host_psk: [u8; 32],
        bundle: Vec<u8>,
    ) -> (String, tokio::sync::oneshot::Receiver<[u8; 32]>) {
        use futures_util::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (key_tx, key_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let join = ws_next(&mut ws).await.unwrap();
            assert_eq!(join.t, FrameType::Join);
            assert_eq!(join.pair_code.as_deref(), Some("devdy-sync-v1/dev-a"));
            if matches!(mode, Mode::Offline) {
                let mut e = Envelope::revoke("");
                e.t = FrameType::Error;
                e.code = Some("pair_invalid".into());
                ws_send(&mut ws, e).await;
                return;
            }
            let mut ready = Envelope::revoke("rm_t");
            ready.t = FrameType::RoomReady;
            ws_send(&mut ws, ready).await;
            let seq = SeqCounter::default();
            let (pending, offer_frame) = offer("rm_t", seq.next()).unwrap();
            ws_send(&mut ws, offer_frame).await;

            let mut frames = Vec::new();
            while frames.len() < 2 {
                let Some(f) = ws_next(&mut ws).await else {
                    return;
                };
                frames.push(f);
            }
            let Some(HandshakePayload::HsAnswer { controller_pub }) =
                crate::device_sync::transport::parse_control(frames[0].cipher.as_deref().unwrap())
            else {
                panic!("expected hs_answer");
            };
            let puller_pub = pairing_unhex(&controller_pub).unwrap();
            assert_eq!(frames[1].t, FrameType::Cmd);
            let mut limiter = FailureLimiter::new();
            let auth = match authorize(
                &mut limiter,
                Instant::now(),
                &host_psk,
                &pending.keypair,
                &puller_pub,
                frames[1].cipher.as_deref().unwrap(),
            ) {
                Ok(a) => a,
                Err(r) => {
                    let reject = HandshakePayload::SyncReject {
                        reason: r.code().into(),
                    };
                    ws_send(&mut ws, control_frame("rm_t", seq.next(), &reject).unwrap()).await;
                    return;
                }
            };
            let salt = random_bytes();
            let key = bundle_key(
                &host_psk,
                &salt,
                &auth.nonce,
                &pending.keypair.public,
                &puller_pub,
            )
            .unwrap();
            let _ = key_tx.send(key);
            let mut out: Vec<Envelope> = bundle_frames(&auth.session, "rm_t", &seq, &salt, &bundle)
                .map(Option::unwrap)
                .collect();
            if matches!(mode, Mode::TamperSha) {
                let start = SyncMsg::Start {
                    v: PROTOCOL_VERSION,
                    salt: pairing_hex(&salt),
                    total: bundle.len() as u64,
                    chunks: chunk_count(bundle.len() as u64),
                    sha256: sha256_hex(b"not the bundle"),
                };
                out[0] = seal_msg(&auth.session, FrameType::Stream, "rm_t", 0, &start).unwrap();
            }
            for f in out {
                ws_send(&mut ws, f).await;
            }
            // Wait for the puller to close.
            while let Some(Ok(_)) = ws.next().await {}
        });
        (url, key_rx)
    }

    fn cats() -> SyncCategories {
        SyncCategories {
            notes: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_pull_downloads_the_bundle_and_derives_the_same_key() {
        let psk = [9u8; 32];
        let bundle: Vec<u8> = (0..(CHUNK_BYTES + 1234)).map(|i| (i % 241) as u8).collect();
        let (url, key_rx) = fake_host(Mode::Serve, psk, bundle.clone()).await;
        let phases = std::sync::Mutex::new(Vec::new());
        let progress = |p: crate::device_sync::transport::Progress| phases.lock().unwrap().push(p);
        let pulled = crate::device_sync::transport::pull(&url, "dev-a", &cats(), &psk, &progress)
            .await
            .unwrap();
        assert_eq!(pulled.bytes, bundle);
        assert_eq!(
            pulled.key,
            key_rx.await.unwrap(),
            "both sides hold the same bundle key"
        );
        let phases = phases.into_inner().unwrap();
        use crate::device_sync::transport::Phase;
        assert_eq!(phases.first().unwrap().phase, Phase::Connecting);
        assert!(phases.iter().any(|p| p.phase == Phase::Auth));
        let last = phases.last().unwrap();
        assert_eq!(
            (last.phase, last.received, last.total),
            (Phase::Downloading, bundle.len() as u64, bundle.len() as u64)
        );
    }

    #[tokio::test]
    async fn a_wrong_password_pull_fails_clearly() {
        let (url, _) = fake_host(Mode::Serve, [9u8; 32], vec![1, 2, 3]).await;
        let err = crate::device_sync::transport::pull(&url, "dev-a", &cats(), &[8u8; 32], &|_| {})
            .await
            .err()
            .unwrap();
        assert_eq!(err, "wrong_password");
    }

    #[tokio::test]
    async fn an_offline_device_fails_clearly() {
        let (url, _) = fake_host(Mode::Offline, [9u8; 32], vec![]).await;
        let err = crate::device_sync::transport::pull(&url, "dev-a", &cats(), &[9u8; 32], &|_| {})
            .await
            .err()
            .unwrap();
        assert_eq!(err, "device_offline");
    }

    #[tokio::test]
    async fn a_corrupted_bundle_fails_the_checksum() {
        let bundle = vec![5u8; 1000];
        let (url, _) = fake_host(Mode::TamperSha, [9u8; 32], bundle).await;
        let err = crate::device_sync::transport::pull(&url, "dev-a", &cats(), &[9u8; 32], &|_| {})
            .await
            .err()
            .unwrap();
        assert_eq!(err, "checksum_mismatch");
    }
}
