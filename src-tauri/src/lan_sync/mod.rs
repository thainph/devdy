//! Pull the whole Devdy configuration from another machine on the same LAN.
//!
//! The device that HAS the data turns sharing on: it listens on a random TCP port, advertises
//! itself over mDNS (`_devdy-sync._tcp`) and shows a 6-digit one-time code. The other device picks
//! it from the discovered list (or types `ip:port`), enters the code, and receives the same bundle
//! a settings backup holds (`settings_backup::export_transfer`); importing it goes through the
//! backup's merge logic. See `protocol.rs` for the SPAKE2 handshake.
//!
//! Sharing is one-shot and short-lived: it stops after one successful transfer, after
//! [`MAX_ATTEMPTS`] wrong codes, or after [`SHARE_TTL`]. Connections are served one at a time.
//! Secrets are included only when the sharing side opts in; they are written to the Keychain by
//! the import exactly like a password-protected backup.

pub mod protocol;

use crate::commands::settings_backup::{self, BackupInfo, ImportReport, ReceivedBackup};
use crate::db::Db;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use protocol::{reason, Hello, Incoming, Op, Pake, Reply};
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

/// How long a share stays open.
const SHARE_TTL: Duration = Duration::from_secs(10 * 60);
/// Wrong codes before the share closes itself (each connection is one online guess).
const MAX_ATTEMPTS: u32 = 5;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Exporting reads every Skill file and Slack thread: give the sharing side time.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Emitted when sharing stops on its own or from the UI.
pub const EVENT_SHARE_ENDED: &str = "lan_sync_share_ended";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    pub code: String,
    pub port: u16,
    /// `ip:port` this device can be reached at, for typing it in when mDNS is blocked.
    pub endpoints: Vec<String>,
    pub device_name: String,
    pub expires_at: String,
    pub include_secrets: bool,
    /// `false` when the mDNS advertisement failed: the other side has to type an endpoint.
    pub advertised: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareEnded {
    /// `sent`, `expired`, `stopped`, `tooManyAttempts`.
    reason: &'static str,
    peer: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanDevice {
    pub id: String,
    pub name: String,
    pub app_version: String,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResult {
    pub peer_name: String,
    pub info: BackupInfo,
}

struct ActiveShare {
    generation: u64,
    status: ShareStatus,
    task: JoinHandle<()>,
    mdns: Option<ServiceDaemon>,
}

pub struct LanSyncState {
    /// Random per launch: lets discovery hide this device's own share.
    device_id: String,
    generation: AtomicU64,
    share: tokio::sync::Mutex<Option<ActiveShare>>,
    received: std::sync::Mutex<Option<(String, ReceivedBackup)>>,
}

impl Default for LanSyncState {
    fn default() -> Self {
        Self {
            device_id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            generation: AtomicU64::new(0),
            share: tokio::sync::Mutex::new(None),
            received: std::sync::Mutex::new(None),
        }
    }
}

impl LanSyncState {
    pub async fn share_status(&self) -> Option<ShareStatus> {
        self.share.lock().await.as_ref().map(|s| s.status.clone())
    }

    pub async fn start_share(
        &self,
        app: &AppHandle,
        include_secrets: bool,
    ) -> Result<ShareStatus, String> {
        let mut slot = self.share.lock().await;
        if let Some(old) = slot.take() {
            close(old, true);
        }

        let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| format!("Could not open a port for sharing: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let device_name = device_name();
        let code = protocol::generate_code();
        let expires_at = chrono::Utc::now()
            + chrono::Duration::from_std(SHARE_TTL).unwrap_or_else(|_| chrono::Duration::zero());
        let mdns = advertise(&self.device_id, &device_name, port);
        let status = ShareStatus {
            code: code.clone(),
            port,
            endpoints: local_ipv4s()
                .into_iter()
                .map(|ip| format!("{ip}:{port}"))
                .collect(),
            device_name: device_name.clone(),
            expires_at: expires_at.to_rfc3339(),
            include_secrets,
            advertised: mdns.is_some(),
        };

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let ctx = ShareCtx {
            app: app.clone(),
            include_secrets,
            guard: Guard {
                code,
                device_name,
                failures: AtomicU32::new(0),
            },
        };
        let task = tokio::spawn(serve(listener, ctx, generation));
        *slot = Some(ActiveShare {
            generation,
            status: status.clone(),
            task,
            mdns,
        });
        Ok(status)
    }

    pub async fn stop_share(&self, app: &AppHandle) {
        if let Some(share) = self.share.lock().await.take() {
            close(share, true);
            emit_ended(app, "stopped", None);
        }
    }

    /// Called by the serving task itself when it is done: no abort, the task is returning.
    async fn finish_share(
        &self,
        app: &AppHandle,
        generation: u64,
        reason: &'static str,
        peer: Option<String>,
    ) {
        let mut slot = self.share.lock().await;
        if slot.as_ref().is_some_and(|s| s.generation == generation) {
            if let Some(share) = slot.take() {
                close(share, false);
            }
            drop(slot);
            emit_ended(app, reason, peer);
        }
    }

    pub async fn discover(&self, wait: Duration) -> Result<Vec<LanDevice>, String> {
        let daemon = ServiceDaemon::new().map_err(|e| format!("Could not start discovery: {e}"))?;
        let rx = daemon
            .browse(protocol::SERVICE_TYPE)
            .map_err(|e| format!("Could not start discovery: {e}"))?;
        let deadline = Instant::now() + wait;
        let mut found: HashMap<String, LanDevice> = HashMap::new();
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv_async()).await {
            let ServiceEvent::ServiceResolved(info) = event else {
                continue;
            };
            let txt = |key: &str| {
                info.get_property_val_str(key)
                    .unwrap_or_default()
                    .to_string()
            };
            let id = txt("id");
            if id.is_empty() || id == self.device_id {
                continue;
            }
            let port = info.get_port();
            let mut ips: Vec<IpAddr> = info
                .get_addresses()
                .iter()
                .filter(|ip| ip.is_ipv4() && !ip.is_loopback())
                .map(|ip| ip.to_ip_addr())
                .collect();
            ips.sort();
            let device = found.entry(id.clone()).or_insert_with(|| LanDevice {
                id,
                name: txt("name"),
                app_version: txt("version"),
                endpoints: Vec::new(),
            });
            for ip in ips {
                let endpoint = SocketAddr::new(ip, port).to_string();
                if !device.endpoints.contains(&endpoint) {
                    device.endpoints.push(endpoint);
                }
            }
        }
        let _ = daemon.shutdown();
        let mut devices: Vec<LanDevice> = found
            .into_values()
            .filter(|d| !d.endpoints.is_empty())
            .collect();
        devices.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(devices)
    }

    /// Connect to a sharing device, prove the code and receive its configuration. The result is
    /// kept here until [`Self::import`] or [`Self::discard`].
    pub async fn fetch(&self, endpoints: &[String], code: &str) -> Result<FetchResult, String> {
        let code = protocol::normalize_code(code).ok_or_else(|| {
            format!(
                "Enter the {}-digit code shown on the other device",
                protocol::CODE_LEN
            )
        })?;
        let mut stream = connect(endpoints).await?;
        let (peer_name, received) = pull(&mut stream, &code).await?;
        let info = received.info();
        *self.received.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((peer_name.clone(), received));
        Ok(FetchResult { peer_name, info })
    }

    pub async fn import(
        &self,
        app: &AppHandle,
        db: &Db,
        create_missing_dirs: bool,
    ) -> Result<ImportReport, String> {
        let (_, received) = self
            .received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| "Nothing received yet: fetch from a device first".to_string())?;
        Ok(settings_backup::import_received(app, db, received, create_missing_dirs).await)
    }

    pub fn discard(&self) {
        self.received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

fn close(share: ActiveShare, abort: bool) {
    if let Some(daemon) = share.mdns {
        let _ = daemon.shutdown();
    }
    if abort {
        share.task.abort();
    }
}

fn emit_ended(app: &AppHandle, reason: &'static str, peer: Option<String>) {
    let _ = app.emit(EVENT_SHARE_ENDED, ShareEnded { reason, peer });
}

// ---- Sharing side --------------------------------------------------------------

struct ShareCtx {
    app: AppHandle,
    include_secrets: bool,
    guard: Guard,
}

/// What a connection is checked against.
struct Guard {
    code: String,
    device_name: String,
    /// Handshakes that did not prove the code.
    failures: AtomicU32,
}

enum Served {
    /// The transfer went through to this peer.
    Sent(String),
    /// A wrong code, a dropped connection or a bad frame: keep listening (unless too many).
    Failed,
}

async fn serve(listener: TcpListener, ctx: ShareCtx, generation: u64) {
    let deadline = Instant::now() + SHARE_TTL;
    let (reason, peer) = loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = tokio::time::sleep_until(deadline) => break ("expired", None),
        };
        let Ok((mut stream, addr)) = accepted else {
            continue;
        };
        match serve_one(&mut stream, &ctx.guard, || export(&ctx)).await {
            Ok(Served::Sent(peer)) => break ("sent", Some(peer)),
            Ok(Served::Failed) | Err(_) => {}
        }
        if ctx.guard.failures.load(Ordering::SeqCst) >= MAX_ATTEMPTS {
            break ("tooManyAttempts", None);
        }
        tracing::debug!("lan sync: connection from {addr} did not complete");
    };
    if let Some(state) = ctx.app.try_state::<LanSyncState>() {
        state.finish_share(&ctx.app, generation, reason, peer).await;
    }
}

async fn serve_one<F, Fut>(
    stream: &mut TcpStream,
    guard: &Guard,
    export: F,
) -> std::io::Result<Served>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<u8>, String>>,
{
    let hello = timeout(
        HANDSHAKE_TIMEOUT,
        protocol::read_frame(stream, protocol::MAX_CONTROL_FRAME),
    )
    .await??;
    let Ok(hello) = serde_json::from_slice::<Hello>(&hello) else {
        return Ok(Served::Failed);
    };
    if hello.proto != protocol::PROTO_VERSION {
        refuse(stream, reason::UNSUPPORTED_VERSION).await?;
        return Ok(Served::Failed);
    }
    let Some(peer_msg) = decode_b64(&hello.pake) else {
        return Ok(Served::Failed);
    };
    // Every handshake that reaches the key exchange is a guess at the code.
    let failures = guard.failures.fetch_add(1, Ordering::SeqCst);
    if failures >= MAX_ATTEMPTS {
        refuse(stream, reason::TOO_MANY_ATTEMPTS).await?;
        return Ok(Served::Failed);
    }
    let (pake, msg) = Pake::server(&guard.code);
    let welcome = Reply::Welcome {
        proto: protocol::PROTO_VERSION,
        device_name: guard.device_name.clone(),
        pake: encode_b64(&msg),
    };
    protocol::write_frame(stream, &serde_json::to_vec(&welcome)?).await?;
    let Ok(channel) = pake.finish(&peer_msg) else {
        return Ok(Served::Failed);
    };

    let request = timeout(
        HANDSHAKE_TIMEOUT,
        protocol::read_frame(stream, protocol::MAX_CONTROL_FRAME),
    )
    .await??;
    let op = match channel.open(&request) {
        Ok(Incoming::Data(data)) => serde_json::from_slice::<Op>(&data).ok(),
        Ok(Incoming::Error(_)) => None,
        Err(()) => {
            protocol::write_frame(stream, &protocol::error_frame(reason::BAD_CODE)).await?;
            return Ok(Served::Failed);
        }
    };
    if op.as_ref().map(|o| o.op.as_str()) != Some(protocol::OP_PULL) {
        return Ok(Served::Failed);
    }
    // The code was right: this guess does not count.
    guard.failures.fetch_sub(1, Ordering::SeqCst);

    let payload = match export().await {
        Ok(payload) => payload,
        Err(e) => {
            tracing::warn!("lan sync: export failed: {e}");
            protocol::write_frame(stream, &protocol::error_frame(reason::EXPORT_FAILED)).await?;
            return Ok(Served::Failed);
        }
    };
    protocol::write_frame(stream, &channel.seal(&payload)).await?;
    let ack = timeout(
        TRANSFER_TIMEOUT,
        protocol::read_frame(stream, protocol::MAX_CONTROL_FRAME),
    )
    .await??;
    match channel.open(&ack) {
        Ok(Incoming::Data(data))
            if serde_json::from_slice::<Op>(&data).is_ok_and(|o| o.op == protocol::OP_ACK) =>
        {
            Ok(Served::Sent(hello.device_name))
        }
        _ => Ok(Served::Failed),
    }
}

async fn export(ctx: &ShareCtx) -> Result<Vec<u8>, String> {
    let db = ctx
        .app
        .try_state::<Db>()
        .ok_or_else(|| "database not ready".to_string())?;
    settings_backup::export_transfer(&ctx.app, db.inner(), ctx.include_secrets).await
}

async fn refuse(stream: &mut TcpStream, reason: &str) -> std::io::Result<()> {
    let reply = Reply::Refused {
        reason: reason.to_string(),
    };
    protocol::write_frame(stream, &serde_json::to_vec(&reply)?).await
}

fn advertise(device_id: &str, device_name: &str, port: u16) -> Option<ServiceDaemon> {
    let daemon = ServiceDaemon::new()
        .map_err(|e| tracing::warn!("lan sync: mDNS unavailable: {e}"))
        .ok()?;
    // Instance names are capped at 63 bytes; the id keeps two machines with the same name apart.
    let label: String = device_name.chars().take(40).collect();
    let instance = format!("{label} ({device_id})");
    let host = format!("devdy-{device_id}.local.");
    let version = env!("CARGO_PKG_VERSION");
    let properties = [
        ("id", device_id),
        ("name", device_name),
        ("version", version),
    ];
    let registered = ServiceInfo::new(
        protocol::SERVICE_TYPE,
        &instance,
        &host,
        "",
        port,
        &properties[..],
    )
    .map(|info| info.enable_addr_auto())
    .and_then(|info| daemon.register(info));
    match registered {
        Ok(()) => Some(daemon),
        Err(e) => {
            tracing::warn!("lan sync: mDNS advertisement failed: {e}");
            let _ = daemon.shutdown();
            None
        }
    }
}

// ---- Receiving side --------------------------------------------------------------

async fn connect(endpoints: &[String]) -> Result<TcpStream, String> {
    let mut last_error = "No address to connect to".to_string();
    for endpoint in endpoints {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            continue;
        }
        match timeout(CONNECT_TIMEOUT, TcpStream::connect(endpoint)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last_error = format!("Could not connect to {endpoint}: {e}"),
            Err(_) => last_error = format!("Could not connect to {endpoint}: timed out"),
        }
    }
    Err(last_error)
}

async fn pull(stream: &mut TcpStream, code: &str) -> Result<(String, ReceivedBackup), String> {
    let io = |e: std::io::Error| format!("Connection to the other device failed: {e}");
    let timed_out = |_| "The other device stopped responding".to_string();

    let (pake, msg) = Pake::client(code);
    let hello = Hello {
        proto: protocol::PROTO_VERSION,
        device_name: device_name(),
        pake: encode_b64(&msg),
    };
    protocol::write_frame(
        stream,
        &serde_json::to_vec(&hello).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(io)?;
    let reply = timeout(
        HANDSHAKE_TIMEOUT,
        protocol::read_frame(stream, protocol::MAX_CONTROL_FRAME),
    )
    .await
    .map_err(timed_out)?
    .map_err(io)?;
    let (peer_name, peer_msg) = match serde_json::from_slice::<Reply>(&reply) {
        Ok(Reply::Welcome {
            device_name, pake, ..
        }) => (
            device_name,
            decode_b64(&pake)
                .ok_or_else(|| "Invalid handshake from the other device".to_string())?,
        ),
        Ok(Reply::Refused { reason }) => return Err(describe(&reason)),
        Err(_) => return Err("The other device is not a Devdy share".to_string()),
    };
    let channel = pake.finish(&peer_msg)?;

    let pull = serde_json::to_vec(&Op {
        op: protocol::OP_PULL.into(),
    })
    .map_err(|e| e.to_string())?;
    protocol::write_frame(stream, &channel.seal(&pull))
        .await
        .map_err(io)?;
    let frame = timeout(
        TRANSFER_TIMEOUT,
        protocol::read_frame(stream, protocol::MAX_DATA_FRAME),
    )
    .await
    .map_err(timed_out)?
    .map_err(io)?;
    let payload = match channel.open(&frame) {
        Ok(Incoming::Data(data)) => data,
        Ok(Incoming::Error(reason)) => return Err(describe(&reason)),
        Err(()) => return Err("The data from the other device could not be decrypted".to_string()),
    };
    let received = ReceivedBackup::parse(&payload)?;
    let ack = serde_json::to_vec(&Op {
        op: protocol::OP_ACK.into(),
    })
    .map_err(|e| e.to_string())?;
    // The data is here: a lost ack only means the other side keeps sharing until it times out.
    let _ = protocol::write_frame(stream, &channel.seal(&ack)).await;
    Ok((peer_name, received))
}

fn describe(reason: &str) -> String {
    match reason {
        reason::BAD_CODE => "Wrong code: check the code shown on the other device".to_string(),
        reason::TOO_MANY_ATTEMPTS => {
            "Too many wrong codes: start sharing again on the other device".to_string()
        }
        reason::UNSUPPORTED_VERSION => {
            "The other device runs an incompatible Devdy version: update both".to_string()
        }
        reason::EXPORT_FAILED => "The other device could not export its data".to_string(),
        other => format!("The other device refused the transfer ({other})"),
    }
}

// ---- Helpers --------------------------------------------------------------------

fn encode_b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn decode_b64(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// The name people know this machine by (macOS "Computer Name"), falling back to the hostname.
pub fn device_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !name.is_empty() {
            return name;
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Devdy".to_string())
}

/// Non-loopback IPv4 addresses of this machine.
fn local_ipv4s() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .map(|iface| iface.ip())
        .filter(|ip| ip.is_ipv4() && !ip.is_unspecified())
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &[u8] =
        br#"{"bundle":{"format":"devdy-settings-backup","version":1,"exported_at":"2026-10-29T00:00:00Z"}}"#;

    fn guard(failures: u32) -> Guard {
        Guard {
            code: "123456".to_string(),
            device_name: "Sharer".to_string(),
            failures: AtomicU32::new(failures),
        }
    }

    /// Serve `connections` connections on loopback; returns the address and the outcomes.
    async fn sharer(
        guard: std::sync::Arc<Guard>,
        connections: usize,
    ) -> (SocketAddr, JoinHandle<Vec<bool>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut sent = Vec::new();
            for _ in 0..connections {
                let (mut stream, _) = listener.accept().await.unwrap();
                let served =
                    serve_one(&mut stream, &guard, || async { Ok(PAYLOAD.to_vec()) }).await;
                sent.push(matches!(served, Ok(Served::Sent(_))));
            }
            sent
        });
        (addr, task)
    }

    #[tokio::test]
    async fn pulls_with_the_right_code_only() {
        let guard = std::sync::Arc::new(guard(0));
        let (addr, task) = sharer(guard.clone(), 2).await;

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let err = pull(&mut stream, "000000").await.err().unwrap();
        assert!(err.starts_with("Wrong code"), "{err}");

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (peer, received) = pull(&mut stream, "123456").await.unwrap();
        assert_eq!(peer, "Sharer");
        assert!(!received.info().has_secrets);

        assert_eq!(task.await.unwrap(), vec![false, true]);
        assert_eq!(guard.failures.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refuses_once_the_attempts_are_used_up() {
        let (addr, task) = sharer(std::sync::Arc::new(guard(MAX_ATTEMPTS)), 1).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let err = pull(&mut stream, "123456").await.err().unwrap();
        assert!(err.starts_with("Too many wrong codes"), "{err}");
        assert_eq!(task.await.unwrap(), vec![false]);
    }

    #[test]
    fn describes_every_reason() {
        for r in [
            reason::BAD_CODE,
            reason::TOO_MANY_ATTEMPTS,
            reason::UNSUPPORTED_VERSION,
            reason::EXPORT_FAILED,
        ] {
            assert!(!describe(r).contains(r), "{r} has no message");
        }
    }
}
