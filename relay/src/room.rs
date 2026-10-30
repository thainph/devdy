//! Routing registry (SRS §6.2, DATA-004) — deliberately minimal.
//!
//! The relay keeps ONLY routing metadata — never payload (CON-04). The model is:
//!
//! - A Host connection **listens** on one rendezvous code per *slot*
//!   (`create_pair`): the default slot carries the bound session's code, the
//!   `login` slot the well-known password-login code. A listen lives exactly as
//!   long as that connection: no TTL, no persistence.
//! - A Controller **joins** a code. The relay opens a room `{host, controller}`
//!   for it and tells both peers `room_ready`. A code has at most ONE room: a
//!   newer join replaces the previous Controller ("latest wins"), which is what
//!   makes a phone that reconnects on a fresh socket — while its old socket is
//!   still half-open — recover instantly instead of being refused.
//! - Any connection that goes away (close, error, liveness timeout) closes the
//!   rooms it was part of and the survivor gets `peer_left`. Peers then simply
//!   re-join; there is no resume token and no room state machine.
//!
//! Authentication is end-to-end between Host and Controller (OTP / session
//! token over the E2E channel), so the relay forwards frames as soon as a room
//! exists — it does not need a HANDSHAKING/ACTIVE distinction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, Notify};

use crate::protocol::Envelope;

/// Unique id for a live WebSocket connection.
pub type ConnId = u64;

/// Frames that may sit queued for one connection before the relay gives up on
/// it. A peer this far behind is not going to catch up, so the connection is
/// killed (its peer gets `peer_left` and both sides re-join) rather than
/// silently dropping frames and leaving the session desynced.
pub const OUTBOUND_MAX_QUEUE: usize = 4096;

/// Sender half used to push envelopes to a connection's write task.
///
/// The channel itself is unbounded — the relay routes from inside a lock and
/// must never await on a send — so the depth is tracked explicitly. Exceeding
/// [`OUTBOUND_MAX_QUEUE`] fires `kill`, which the connection task turns into a
/// disconnect.
/// One queued frame: a routed/control envelope, or a frame the sync store
/// already serialized (its shape is not an [`Envelope`]).
// Envelopes are the hot path (every routed frame); boxing them to shrink the
// rare text variant would add an allocation per frame.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum OutFrame {
    Env(Envelope),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Outbound {
    tx: mpsc::UnboundedSender<OutFrame>,
    depth: Arc<AtomicUsize>,
    kill: Arc<Notify>,
}

/// Receiver half, owned by a connection's write task.
pub struct OutboundRx {
    rx: mpsc::UnboundedReceiver<OutFrame>,
    depth: Arc<AtomicUsize>,
}

/// Create a connected pair for one connection.
pub fn outbound_channel() -> (Outbound, OutboundRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    let depth = Arc::new(AtomicUsize::new(0));
    (
        Outbound {
            tx,
            depth: depth.clone(),
            kill: Arc::new(Notify::new()),
        },
        OutboundRx { rx, depth },
    )
}

impl Outbound {
    /// Queue a frame. `Err(())` when the write task is gone OR the peer is too
    /// far behind; in the latter case the connection is also signalled to close.
    #[allow(clippy::result_unit_err)]
    pub fn send(&self, env: Envelope) -> Result<(), ()> {
        self.send_frame(OutFrame::Env(env))
    }

    /// Queue an already-serialized JSON text frame (sync responses).
    #[allow(clippy::result_unit_err)]
    pub fn send_text(&self, text: String) -> Result<(), ()> {
        self.send_frame(OutFrame::Text(text))
    }

    fn send_frame(&self, frame: OutFrame) -> Result<(), ()> {
        if self.depth.load(Ordering::Relaxed) >= OUTBOUND_MAX_QUEUE {
            self.kill.notify_one();
            return Err(());
        }
        match self.tx.send(frame) {
            Ok(()) => {
                self.depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => Err(()),
        }
    }

    /// Ask this connection's task to close (after flushing what is queued).
    pub fn close(&self) {
        self.kill.notify_one();
    }

    /// Resolves once this connection has been asked to close (too slow, or
    /// superseded).
    pub async fn killed(&self) {
        self.kill.notified().await
    }

    /// Frames queued but not yet written. Exercised by the unit tests.
    #[allow(dead_code)]
    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
}

impl OutboundRx {
    /// Await the next frame, keeping the depth gauge in step.
    pub async fn recv(&mut self) -> Option<OutFrame> {
        let env = self.rx.recv().await?;
        self.depth.fetch_sub(1, Ordering::Relaxed);
        Some(env)
    }

    /// Take an already-queued frame without awaiting. Exercised by the tests.
    #[allow(dead_code, clippy::result_unit_err)]
    pub fn try_recv(&mut self) -> Result<OutFrame, ()> {
        match self.rx.try_recv() {
            Ok(env) => {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                Ok(env)
            }
            Err(_) => Err(()),
        }
    }
}

/// A peer that must be told a room closed, and how.
#[derive(Debug)]
pub struct Notice {
    pub room_id: String,
    pub out: Outbound,
    /// `true` when the recipient is a Controller that was replaced by a newer
    /// join on the same code: it gets `error{superseded}` instead of
    /// `peer_left`, so it stops instead of re-joining and kicking the newcomer
    /// back (two tabs ping-ponging forever).
    pub superseded: bool,
}

/// One live room: exactly one Host and one Controller.
struct Room {
    code: u64,
    host: ConnId,
    host_out: Outbound,
    controller: ConnId,
    controller_out: Outbound,
}

/// A Host connection listening on a rendezvous code.
struct Listener {
    host: ConnId,
    host_out: Outbound,
    /// Which of the Host's slots this code fills ("" = the session slot).
    slot: String,
    /// Room id the next join on this code will get. Announced to the Host in the
    /// `create_pair` ack so older Hosts (which track "armed" rooms) keep working.
    next_room_id: String,
}

/// Outcome of a `join`.
pub enum JoinOutcome {
    /// Room opened. The caller sends `room_ready` to both sinks and delivers
    /// `notices` (the replaced room, if any).
    Joined {
        room_id: String,
        host_out: Outbound,
        /// The listener's slot, echoed to the Host in `room_ready`.
        slot: String,
        notices: Vec<Notice>,
    },
    /// No Host is listening on that code (offline, or the link was superseded).
    Invalid,
}

/// In-memory registry of listeners and rooms. Guarded by a single `Mutex` in
/// the server; panic-free (all lookups return `Option`).
#[derive(Default)]
pub struct RoomRegistry {
    /// code hash → listening Host.
    listeners: HashMap<u64, Listener>,
    /// room id → room.
    rooms: HashMap<String, Room>,
    /// (Host conn, slot) → the code it listens on there.
    host_codes: HashMap<(ConnId, String), u64>,
    /// code hash → its (at most one) room.
    room_by_code: HashMap<u64, String>,
    /// Controller conn → the room it is in (at most one). A Host's rooms are
    /// always the rooms of the codes it listens on, found via `host_codes`.
    room_by_ctrl: HashMap<ConnId, String>,
}

impl RoomRegistry {
    /// Whether a Host already listens on this code (a re-listen does not grow
    /// the registry, so it is exempt from the cap).
    pub fn is_listening(&self, code: u64) -> bool {
        self.listeners.contains_key(&code)
    }

    /// Number of listeners + rooms, for the resource cap.
    pub fn len(&self) -> usize {
        self.listeners.len() + self.rooms.len()
    }

    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.listeners.is_empty() && self.rooms.is_empty()
    }

    /// Make `host` listen on `code` in `slot`, replacing what that slot
    /// listened on before.
    ///
    /// - Same Host, same code (a re-announce): only the pre-assigned room id is
    ///   refreshed; a live room on that code is untouched.
    /// - Same Host + slot, new code (session superseded on the desktop): the old
    ///   code is dropped and its room closed.
    /// - Another connection held this code (a stale socket of the same Host that
    ///   has not timed out yet): the newer Host takes it over and the old room
    ///   is closed so its Controller re-joins the live Host.
    ///
    /// Returns the peers to notify.
    pub fn listen(&mut self, code: u64, host: ConnId, host_out: Outbound, next_room_id: String, slot: &str) -> Vec<Notice> {
        let mut notices = Vec::new();
        let key = (host, slot.to_string());
        if let Some(prev) = self.host_codes.get(&key).copied() {
            if prev != code {
                notices.extend(self.unlisten(prev, Some(host)));
            }
        }
        if self.listeners.get(&code).is_some_and(|l| l.host != host || l.slot != slot) {
            notices.extend(self.unlisten(code, Some(host)));
        }
        self.listeners.insert(
            code,
            Listener {
                host,
                host_out,
                slot: slot.to_string(),
                next_room_id,
            },
        );
        self.host_codes.insert(key, code);
        notices
    }

    /// Stop listening on `code` and close its room. `originator` is excluded
    /// from the notices.
    fn unlisten(&mut self, code: u64, originator: Option<ConnId>) -> Vec<Notice> {
        if let Some(l) = self.listeners.remove(&code) {
            let key = (l.host, l.slot);
            if self.host_codes.get(&key) == Some(&code) {
                self.host_codes.remove(&key);
            }
        }
        match self.room_by_code.get(&code).cloned() {
            Some(room_id) => self.close_room(&room_id, originator),
            None => Vec::new(),
        }
    }

    /// A Controller joins `code`. `next_room_id` is the id the FOLLOWING join
    /// will get (this join consumes the listener's pre-assigned one).
    pub fn join(&mut self, code: u64, controller: ConnId, controller_out: Outbound, next_room_id: String) -> JoinOutcome {
        if !self.listeners.contains_key(&code) {
            return JoinOutcome::Invalid;
        }
        let mut notices = Vec::new();
        // A connection is in at most one room: leaving the old one first.
        if let Some(old) = self.room_by_ctrl.get(&controller).cloned() {
            notices.extend(self.close_room(&old, Some(controller)));
        }
        // Latest wins: replace the code's current Controller.
        if let Some(old) = self.room_by_code.get(&code).cloned() {
            let old_ctrl_out = self.rooms.get(&old).map(|r| r.controller_out.clone());
            for mut n in self.close_room(&old, None) {
                n.superseded = old_ctrl_out.as_ref().is_some_and(|o| same_sink(&n.out, o));
                notices.push(n);
            }
        }
        let Some(l) = self.listeners.get_mut(&code) else {
            return JoinOutcome::Invalid;
        };
        let room_id = std::mem::replace(&mut l.next_room_id, next_room_id);
        let host = l.host;
        let host_out = l.host_out.clone();
        let slot = l.slot.clone();
        self.rooms.insert(
            room_id.clone(),
            Room {
                code,
                host,
                host_out: host_out.clone(),
                controller,
                controller_out,
            },
        );
        self.room_by_code.insert(code, room_id.clone());
        self.room_by_ctrl.insert(controller, room_id.clone());
        JoinOutcome::Joined {
            room_id,
            host_out,
            slot,
            notices,
        }
    }

    /// Where a data frame from `sender` in `room_id` goes: the other member, or
    /// `None` when the room is gone or the sender is not in it.
    pub fn route(&self, room_id: &str, sender: ConnId) -> Option<Outbound> {
        let room = self.rooms.get(room_id)?;
        if room.host == sender {
            Some(room.controller_out.clone())
        } else if room.controller == sender {
            Some(room.host_out.clone())
        } else {
            None
        }
    }

    /// Host-initiated close. An empty `room_id` means "stop listening in
    /// `slot`" (end the session / disable password login) and closes that
    /// slot's room. Otherwise close that room if the Host owns it. Older Hosts
    /// revoke by sending their rendezvous code as the room id; `room_id_hash`
    /// (its code hash) lets that end the session too.
    /// Returns the peers to notify.
    pub fn revoke(&mut self, host: ConnId, room_id: &str, room_id_hash: u64, slot: &str) -> Vec<Notice> {
        if room_id.is_empty() {
            return match self.host_codes.get(&(host, slot.to_string())).copied() {
                Some(code) => self.unlisten(code, Some(host)),
                None => Vec::new(),
            };
        }
        if self.listeners.get(&room_id_hash).is_some_and(|l| l.host == host) {
            return self.unlisten(room_id_hash, Some(host));
        }
        match self.rooms.get(room_id) {
            Some(r) if r.host == host => self.close_room(room_id, Some(host)),
            _ => Vec::new(),
        }
    }

    /// Close a room, returning every member except `originator` to notify.
    fn close_room(&mut self, room_id: &str, originator: Option<ConnId>) -> Vec<Notice> {
        let Some(room) = self.rooms.remove(room_id) else {
            return Vec::new();
        };
        if self.room_by_code.get(&room.code).map(String::as_str) == Some(room_id) {
            self.room_by_code.remove(&room.code);
        }
        if self.room_by_ctrl.get(&room.controller).map(String::as_str) == Some(room_id) {
            self.room_by_ctrl.remove(&room.controller);
        }
        let mut out = Vec::new();
        if Some(room.host) != originator {
            out.push(Notice {
                room_id: room_id.to_string(),
                out: room.host_out,
                superseded: false,
            });
        }
        if Some(room.controller) != originator {
            out.push(Notice {
                room_id: room_id.to_string(),
                out: room.controller_out,
                superseded: false,
            });
        }
        out
    }

    /// A connection went away: drop its listens (if a Host) — which closes
    /// their rooms — or close its room (if a Controller).
    pub fn on_disconnect(&mut self, conn: ConnId) -> Vec<Notice> {
        let mut notices = Vec::new();
        let codes: Vec<u64> = self
            .host_codes
            .iter()
            .filter(|((c, _), _)| *c == conn)
            .map(|(_, code)| *code)
            .collect();
        for code in codes {
            notices.extend(self.unlisten(code, Some(conn)));
        }
        if let Some(room_id) = self.room_by_ctrl.get(&conn).cloned() {
            notices.extend(self.close_room(&room_id, Some(conn)));
        }
        notices
    }

    /// Whether `conn` is currently a member of `room_id` (tests/introspection).
    pub fn in_room(&self, room_id: &str, conn: ConnId) -> bool {
        self.rooms
            .get(room_id)
            .is_some_and(|r| r.host == conn || r.controller == conn)
    }
}

/// Whether two senders feed the same connection.
fn same_sink(a: &Outbound, b: &Outbound) -> bool {
    Arc::ptr_eq(&a.depth, &b.depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> (Outbound, OutboundRx) {
        outbound_channel()
    }

    fn listen(reg: &mut RoomRegistry, code: u64, host: ConnId) -> Outbound {
        let (out, _rx) = sink();
        assert!(reg.listen(code, host, out.clone(), format!("rm_{host}_0"), "").is_empty());
        out
    }

    fn joined(o: JoinOutcome) -> (String, Vec<Notice>) {
        match o {
            JoinOutcome::Joined { room_id, notices, .. } => (room_id, notices),
            JoinOutcome::Invalid => panic!("expected Joined"),
        }
    }

    #[test]
    fn join_unknown_code_is_invalid() {
        let mut reg = RoomRegistry::default();
        assert!(matches!(reg.join(1, 20, sink().0, "x".into()), JoinOutcome::Invalid));
    }

    #[test]
    fn join_opens_a_room_that_routes_both_ways() {
        let mut reg = RoomRegistry::default();
        let host_out = listen(&mut reg, 1, 10);
        let (ctrl_out, _rx) = sink();
        let (room, notices) = joined(reg.join(1, 20, ctrl_out.clone(), "rm_next".into()));
        assert_eq!(room, "rm_10_0", "the pre-assigned id is used");
        assert!(notices.is_empty());
        assert!(same_sink(&reg.route(&room, 10).unwrap(), &ctrl_out));
        assert!(same_sink(&reg.route(&room, 20).unwrap(), &host_out));
        assert!(reg.route(&room, 99).is_none(), "non-members cannot route");
    }

    #[test]
    fn newer_join_replaces_the_controller_and_supersedes_the_old_one() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 1, 10);
        let (old_ctrl, _r1) = sink();
        let (room_a, _) = joined(reg.join(1, 20, old_ctrl.clone(), "rm_b".into()));
        let (room_b, notices) = joined(reg.join(1, 21, sink().0, "rm_c".into()));
        assert_eq!(room_b, "rm_b");
        assert!(!reg.in_room(&room_a, 20), "old room closed");
        assert_eq!(notices.len(), 2, "host and old controller are told");
        let old = notices.iter().find(|n| same_sink(&n.out, &old_ctrl)).unwrap();
        assert!(old.superseded, "old controller is superseded, not peer_left");
        assert!(notices.iter().any(|n| !n.superseded && n.room_id == room_a), "host gets peer_left");
        assert!(reg.in_room(&room_b, 10) && reg.in_room(&room_b, 21));
    }

    #[test]
    fn controller_disconnect_keeps_the_listen_so_it_can_rejoin() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 1, 10);
        let (room, _) = joined(reg.join(1, 20, sink().0, "rm_2".into()));
        let notices = reg.on_disconnect(20);
        assert_eq!(notices.len(), 1, "host gets peer_left");
        assert_eq!(notices[0].room_id, room);
        assert!(reg.is_listening(1), "host still listening");
        assert!(matches!(reg.join(1, 21, sink().0, "rm_3".into()), JoinOutcome::Joined { .. }));
    }

    #[test]
    fn host_disconnect_clears_everything_it_owned() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 1, 10);
        let (ctrl_out, _rx) = sink();
        let (room, _) = joined(reg.join(1, 20, ctrl_out.clone(), "rm_2".into()));
        let notices = reg.on_disconnect(10);
        assert_eq!(notices.len(), 1);
        assert!(same_sink(&notices[0].out, &ctrl_out));
        assert!(!notices[0].superseded);
        assert!(!reg.in_room(&room, 20));
        assert!(reg.is_empty(), "no leaked listener or room");
        assert!(matches!(reg.join(1, 21, sink().0, "x".into()), JoinOutcome::Invalid));
    }

    #[test]
    fn reannouncing_the_same_code_keeps_the_live_room() {
        let mut reg = RoomRegistry::default();
        let (host_out, _rx) = sink();
        reg.listen(1, 10, host_out.clone(), "rm_a".into(), "");
        let (room, _) = joined(reg.join(1, 20, sink().0, "rm_b".into()));
        assert!(reg.listen(1, 10, host_out, "rm_c".into(), "").is_empty());
        assert!(reg.in_room(&room, 20), "room untouched by re-announce");
        assert_eq!(reg.len(), 2, "one listener + one room");
    }

    #[test]
    fn new_code_from_same_host_drops_the_old_session() {
        let mut reg = RoomRegistry::default();
        let host_out = listen(&mut reg, 1, 10);
        let (room, _) = joined(reg.join(1, 20, sink().0, "rm_b".into()));
        let notices = reg.listen(2, 10, host_out, "rm_c".into(), "");
        assert_eq!(notices.len(), 1, "old controller told, host (originator) not");
        assert!(!reg.in_room(&room, 20));
        assert!(!reg.is_listening(1));
        assert!(reg.is_listening(2));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn a_fresh_host_socket_takes_over_a_stale_one() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 1, 10);
        let (room, _) = joined(reg.join(1, 20, sink().0, "rm_b".into()));
        // Host reconnected on conn 11 while conn 10 is half-open.
        let (new_host, _rx) = sink();
        let notices = reg.listen(1, 11, new_host.clone(), "rm_c".into(), "");
        assert!(notices.iter().any(|n| n.room_id == room), "controller told to re-join");
        let (room2, _) = joined(reg.join(1, 20, sink().0, "rm_d".into()));
        assert!(same_sink(&reg.route(&room2, 20).unwrap(), &new_host));
        // The stale socket finally dies: it must not tear down the new listen.
        assert!(reg.on_disconnect(10).is_empty());
        assert!(reg.is_listening(1));
        assert!(reg.in_room(&room2, 11));
    }

    #[test]
    fn revoke_empty_ends_the_session() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 1, 10);
        let (room, _) = joined(reg.join(1, 20, sink().0, "rm_b".into()));
        assert!(reg.revoke(20, "", 0, "").is_empty(), "a controller cannot revoke");
        assert_eq!(reg.revoke(10, "", 0, "").len(), 1);
        assert!(!reg.in_room(&room, 20));
        assert!(reg.is_empty());
    }

    #[test]
    fn revoke_a_room_keeps_listening() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 1, 10);
        let (room, _) = joined(reg.join(1, 20, sink().0, "rm_b".into()));
        assert!(reg.revoke(20, &room, 0, "").is_empty(), "only the host may revoke");
        assert_eq!(reg.revoke(10, &room, 0, "").len(), 1);
        assert!(reg.is_listening(1));
    }

    #[test]
    fn login_slot_lives_beside_the_session_slot() {
        let mut reg = RoomRegistry::default();
        let host_out = listen(&mut reg, 1, 10);
        assert!(reg.listen(9, 10, host_out.clone(), "rm_l0".into(), "login").is_empty());
        let (session_room, _) = joined(reg.join(1, 20, sink().0, "rm_s1".into()));
        match reg.join(9, 21, sink().0, "rm_l1".into()) {
            JoinOutcome::Joined { room_id, slot, .. } => {
                assert_eq!(slot, "login");
                assert!(reg.in_room(&room_id, 10));
            }
            JoinOutcome::Invalid => panic!("login slot must be joinable"),
        }
        // Ending the session leaves password login available.
        assert_eq!(reg.revoke(10, "", 0, "").len(), 1);
        assert!(!reg.in_room(&session_room, 20));
        assert!(reg.is_listening(9));
        // Disabling login is its own revoke.
        assert_eq!(reg.revoke(10, "", 0, "login").len(), 1);
        assert!(reg.is_empty());
    }

    #[test]
    fn host_disconnect_drops_every_slot() {
        let mut reg = RoomRegistry::default();
        let host_out = listen(&mut reg, 1, 10);
        reg.listen(9, 10, host_out, "rm_l0".into(), "login");
        let _ = joined(reg.join(1, 20, sink().0, "a".into()));
        let _ = joined(reg.join(9, 21, sink().0, "b".into()));
        assert_eq!(reg.on_disconnect(10).len(), 2, "both controllers told");
        assert!(reg.is_empty());
    }

    #[test]
    fn legacy_revoke_by_code_ends_the_session() {
        let mut reg = RoomRegistry::default();
        listen(&mut reg, 7, 10);
        let _ = joined(reg.join(7, 20, sink().0, "rm_b".into()));
        assert_eq!(reg.revoke(10, "the-code", 7, "").len(), 1);
        assert!(reg.is_empty());
    }

    #[test]
    fn many_reconnects_never_grow_the_registry() {
        let mut reg = RoomRegistry::default();
        let host_out = listen(&mut reg, 1, 10);
        for i in 0..100u64 {
            let ctrl = 100 + i;
            let _ = reg.join(1, ctrl, sink().0, format!("rm_{i}"));
            reg.listen(1, 10, host_out.clone(), format!("rm_x{i}"), "");
            if i % 2 == 0 {
                reg.on_disconnect(ctrl);
            }
        }
        assert!(reg.len() <= 2, "bounded: one listener + at most one room");
    }

    #[test]
    fn outbound_kills_a_peer_that_falls_too_far_behind() {
        let (tx, mut rx) = outbound_channel();
        for i in 0..OUTBOUND_MAX_QUEUE {
            assert!(tx.send(Envelope::peer_left(format!("rm_{i}"))).is_ok());
        }
        assert!(tx.send(Envelope::peer_left("rm_over")).is_err());
        // The kill permit is stored, so a later waiter still sees it.
        futures_util::FutureExt::now_or_never(tx.killed()).expect("kill signalled");
        assert!(rx.try_recv().is_ok());
        assert!(tx.send(Envelope::peer_left("rm_ok")).is_ok());
    }
}
