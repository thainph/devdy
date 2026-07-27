//! Room state machine and registry (SRS §6.2, DATA-004).
//!
//! The relay keeps ONLY routing metadata — never payload (CON-04). Each room
//! ties one Host to at most one Controller (MAX_CONTROLLER=1, BR-015).
//!
//! State machine:
//! ```text
//! PENDING_PAIR --join valid--> HANDSHAKING --both handshake_done--> ACTIVE
//! PENDING_PAIR --TTL elapsed--> EXPIRED (terminal)
//! HANDSHAKING  --fail--------> CLOSED  (terminal)
//! ACTIVE       --revoke/host left/idle--> CLOSED (terminal)
//! ACTIVE       --controller drops--> ACTIVE (controller away, resume_deadline)
//! ACTIVE(away) --resume valid--> HANDSHAKING (re-run E2E, rotate token)
//! ACTIVE(away) --window elapsed--> CLOSED (terminal)
//! ```
//!
//! Controller reconnect (phone screen off/on): when the Controller drops from an
//! ACTIVE room the room is NOT closed immediately — it keeps the Host attached
//! and holds a `resume_deadline` (Host.reconnect_window). A `resume` frame
//! carrying the current rotating `resume_token` re-attaches a fresh Controller
//! connection and re-runs the handshake. The token rotates on every join/resume
//! so a stale token can never be replayed.

use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::protocol::Envelope;

/// Unique id for a live WebSocket connection.
pub type ConnId = u64;

/// Sender half used to push envelopes to a connection's write task.
pub type Outbound = mpsc::UnboundedSender<Envelope>;

/// Lifecycle state of a room (SRS §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomState {
    /// Host created a `pair_code`; waiting for a Controller `join`.
    PendingPair,
    /// Controller joined; peers are running the E2E handshake.
    Handshaking,
    /// Handshake complete; envelopes are forwarded between peers.
    Active,
    /// Terminal: pair code TTL elapsed before use.
    Expired,
    /// Terminal: revoked, both peers gone, handshake failed, or idle timeout.
    Closed,
}

impl RoomState {
    /// Whether the state is terminal (no outbound transitions).
    pub fn is_terminal(self) -> bool {
        matches!(self, RoomState::Expired | RoomState::Closed)
    }
}

/// One side of a room.
#[derive(Debug, Clone)]
struct Peer {
    conn: ConnId,
    out: Outbound,
    handshake_done: bool,
}

/// A single room's metadata (DATA-004). No payload is stored here.
pub struct Room {
    pub room_id: String,
    pub state: RoomState,
    /// Hash of the pair code (never the raw code) while PENDING_PAIR.
    pub pair_code_hash: u64,
    pub created_at: Instant,
    /// Absolute deadline for the pending pair code (created_at + PAIR_TTL).
    pub expires_at: Instant,
    /// Last time any data frame moved through the room (for idle timeout).
    pub last_activity: Instant,
    /// Hash of the current rotating resume token, once a Controller has joined.
    resume_token_hash: Option<u64>,
    /// While the Controller is away (dropped but resumable), the absolute
    /// deadline after which the room is swept. `None` while a Controller is
    /// attached.
    resume_deadline: Option<Instant>,
    host: Peer,
    controller: Option<Peer>,
}

impl Room {
    /// The `room_id` currently assigned.
    pub fn id(&self) -> &str {
        &self.room_id
    }

    /// Whether both peers have reported their handshake half complete.
    fn both_handshaken(&self) -> bool {
        self.host.handshake_done
            && self
                .controller
                .as_ref()
                .map(|c| c.handshake_done)
                .unwrap_or(false)
    }
}

/// The set of peers a routed frame should be delivered to, plus the resolved
/// target role, so the caller can log without touching payload.
pub struct RouteTargets {
    /// Outbound channels of every peer other than the sender.
    pub sinks: Vec<Outbound>,
}

/// In-memory registry of all rooms and the reverse indexes needed to route and
/// clean up by connection id.
///
/// Guarded by a single `Mutex` in the server; kept intentionally simple and
/// panic-free (all lookups return `Option`).
#[derive(Default)]
pub struct RoomRegistry {
    rooms: HashMap<String, Room>,
    /// pair_code_hash -> room_id, for pending rooms only.
    pending_by_code: HashMap<u64, String>,
    /// resume_token_hash -> room_id, for ACTIVE (incl. controller-away) rooms.
    resume_by_token: HashMap<u64, String>,
    /// conn id -> room_id, for both host and controller connections.
    conn_to_room: HashMap<ConnId, String>,
}

/// Outcome of a Controller `join` attempt.
pub enum JoinOutcome {
    /// Joined successfully; carries the new room id and both outbound sinks so
    /// the caller can emit `room_ready` to both peers.
    Joined {
        room_id: String,
        host_out: Outbound,
        controller_out: Outbound,
    },
    /// The pair code is unknown, already consumed, or expired.
    Invalid,
    /// The room already holds its one allowed Controller (MAX_CONTROLLER=1).
    Full,
}

/// Outcome of a Controller `resume` attempt (reconnect within the window).
pub enum ResumeOutcome {
    /// Re-attached; the room is back in HANDSHAKING and both peers must re-run
    /// the E2E handshake. Carries both sinks so the caller can emit `room_ready`
    /// (the Host re-offers on receipt; the Controller learns its new token).
    Resumed {
        room_id: String,
        host_out: Outbound,
        controller_out: Outbound,
    },
    /// The token is unknown/rotated, the window elapsed, or a Controller is
    /// already attached to that room.
    Invalid,
}

impl RoomRegistry {
    /// Register a Host-created pair code in a fresh PENDING_PAIR room.
    ///
    /// Returns the generated `room_id`. The raw pair code is hashed by the
    /// caller; only the hash is kept (DATA-004: `pair_code(hash)`).
    #[allow(clippy::too_many_arguments)]
    pub fn create_pending(
        &mut self,
        room_id: String,
        pair_code_hash: u64,
        host_conn: ConnId,
        host_out: Outbound,
        now: Instant,
        expires_at: Instant,
    ) {
        // A host re-arms its standing rendezvous by announcing the same pair code
        // again (it does so after every controller (re)connect). The previous
        // PENDING rendezvous for this code is now superseded — close it so a
        // long-lived host does not accumulate orphaned persistent rooms: they
        // carry a ~10-year expiry and would otherwise never be swept, leaking a
        // Room (and its outbound sender) on every reconnect. Keyed on
        // `pending_by_code`, which only ever references PENDING rooms, so an
        // ACTIVE/HANDSHAKING session (removed from this index on join) is never
        // touched. Pass the host as originator so no `peer_left` is emitted — this
        // is a silent supersede, not a teardown the host should react to.
        if let Some(prev_id) = self.pending_by_code.get(&pair_code_hash).cloned() {
            if prev_id != room_id {
                let _ = self.close_room(&prev_id, Some(host_conn));
            }
        }
        let room = Room {
            room_id: room_id.clone(),
            state: RoomState::PendingPair,
            pair_code_hash,
            created_at: now,
            expires_at,
            last_activity: now,
            resume_token_hash: None,
            resume_deadline: None,
            host: Peer {
                conn: host_conn,
                out: host_out,
                handshake_done: false,
            },
            controller: None,
        };
        self.pending_by_code.insert(pair_code_hash, room_id.clone());
        self.conn_to_room.insert(host_conn, room_id.clone());
        self.rooms.insert(room_id, room);
    }

    /// Attempt a Controller join by pair code hash (SRS §6.2 PENDING_PAIR→HANDSHAKING).
    ///
    /// `resume_token_hash` is the hash of a freshly minted resume token the
    /// caller will hand to the Controller; it is stored so a later `resume` can
    /// re-attach to this room.
    pub fn join(
        &mut self,
        pair_code_hash: u64,
        controller_conn: ConnId,
        controller_out: Outbound,
        resume_token_hash: u64,
        now: Instant,
    ) -> JoinOutcome {
        let room_id = match self.pending_by_code.get(&pair_code_hash).cloned() {
            Some(id) => id,
            None => return JoinOutcome::Invalid,
        };

        let room = match self.rooms.get_mut(&room_id) {
            Some(r) => r,
            None => {
                // Dangling index; clean it up.
                self.pending_by_code.remove(&pair_code_hash);
                return JoinOutcome::Invalid;
            }
        };

        // Expired-by-time or no longer pending → invalid (do not create a room).
        if room.state != RoomState::PendingPair || now >= room.expires_at {
            if now >= room.expires_at && room.state == RoomState::PendingPair {
                room.state = RoomState::Expired;
            }
            self.pending_by_code.remove(&pair_code_hash);
            return JoinOutcome::Invalid;
        }

        // MAX_CONTROLLER=1: a second controller is refused (BR-015 / AC-16).
        if room.controller.is_some() {
            return JoinOutcome::Full;
        }

        room.controller = Some(Peer {
            conn: controller_conn,
            out: controller_out.clone(),
            handshake_done: false,
        });
        room.state = RoomState::Handshaking;
        room.last_activity = now;
        room.resume_token_hash = Some(resume_token_hash);
        room.resume_deadline = None;
        // Pair code is now consumed (one-time, BR-002).
        self.pending_by_code.remove(&pair_code_hash);
        self.resume_by_token.insert(resume_token_hash, room_id.clone());
        self.conn_to_room.insert(controller_conn, room_id.clone());

        let host_out = room.host.out.clone();
        JoinOutcome::Joined {
            room_id,
            host_out,
            controller_out,
        }
    }

    /// Attempt a Controller `resume` by presenting the current resume token hash
    /// (reconnect within the window). On success the room returns to HANDSHAKING
    /// with a rotated token so both peers re-run the E2E handshake.
    ///
    /// `new_token_hash` replaces the redeemed token (rotation / anti-replay).
    pub fn resume(
        &mut self,
        old_token_hash: u64,
        new_token_hash: u64,
        controller_conn: ConnId,
        controller_out: Outbound,
        now: Instant,
    ) -> ResumeOutcome {
        let room_id = match self.resume_by_token.get(&old_token_hash).cloned() {
            Some(id) => id,
            None => return ResumeOutcome::Invalid,
        };
        let room = match self.rooms.get_mut(&room_id) {
            Some(r) => r,
            None => {
                self.resume_by_token.remove(&old_token_hash);
                return ResumeOutcome::Invalid;
            }
        };

        // Only an ACTIVE room whose Controller is currently away may be resumed,
        // and only before the reconnect window elapses.
        let within_window = room.resume_deadline.map(|d| now < d).unwrap_or(false);
        if room.state != RoomState::Active || room.controller.is_some() || !within_window {
            return ResumeOutcome::Invalid;
        }

        // Re-attach the fresh Controller connection; both peers must re-handshake.
        room.host.handshake_done = false;
        room.controller = Some(Peer {
            conn: controller_conn,
            out: controller_out.clone(),
            handshake_done: false,
        });
        room.state = RoomState::Handshaking;
        room.resume_deadline = None;
        room.last_activity = now;
        // Rotate the token so the redeemed one can never be replayed.
        room.resume_token_hash = Some(new_token_hash);
        self.resume_by_token.remove(&old_token_hash);
        self.resume_by_token.insert(new_token_hash, room_id.clone());
        self.conn_to_room.insert(controller_conn, room_id.clone());

        let host_out = room.host.out.clone();
        ResumeOutcome::Resumed {
            room_id,
            host_out,
            controller_out,
        }
    }

    /// Mark a connection's handshake half done. Returns the room id and whether
    /// the room just transitioned to ACTIVE.
    ///
    /// Routed by the frame's explicit `room_id` (NOT `conn_to_room`): a single
    /// Host connection legitimately belongs to several rooms at once — the ACTIVE
    /// session plus any standing durable-reconnect rendezvous it has re-armed — so
    /// `conn_to_room[host]` points only at the most-recently created one. Using it
    /// here would mark the wrong room and leave the real one stuck in HANDSHAKING
    /// (durable reconnect never reaching ACTIVE). The sender must be a member of
    /// the named room.
    pub fn mark_handshake_done(
        &mut self,
        room_id: &str,
        conn: ConnId,
        now: Instant,
    ) -> Option<(String, bool)> {
        let room = self.rooms.get_mut(room_id)?;
        let is_host = room.host.conn == conn;
        let is_ctrl = room
            .controller
            .as_ref()
            .map(|c| c.conn == conn)
            .unwrap_or(false);
        if !is_host && !is_ctrl {
            return None;
        }
        if room.state != RoomState::Handshaking {
            return Some((room_id.to_string(), room.state == RoomState::Active));
        }
        if is_host {
            room.host.handshake_done = true;
        } else if let Some(c) = room.controller.as_mut() {
            c.handshake_done = true;
        }
        let became_active = room.both_handshaken();
        if became_active {
            room.state = RoomState::Active;
            room.last_activity = now;
        }
        Some((room_id.to_string(), became_active))
    }

    /// Resolve where a data frame from `sender_conn` in `room_id` should go.
    ///
    /// Permitted while the room is ACTIVE. When `allow_handshaking` is set (used
    /// only for E2E handshake `control` frames), routing is also allowed while
    /// the room is HANDSHAKING so the offer/answer can be exchanged before the
    /// room becomes ACTIVE — otherwise the handshake could never complete.
    /// Returns `None` when not permitted. Does not inspect any payload.
    pub fn route(
        &mut self,
        room_id: &str,
        sender_conn: ConnId,
        now: Instant,
        allow_handshaking: bool,
    ) -> Option<RouteTargets> {
        let room = self.rooms.get_mut(room_id)?;
        let permitted = room.state == RoomState::Active
            || (allow_handshaking && room.state == RoomState::Handshaking);
        if !permitted {
            return None;
        }
        // The sender must be a member of this room.
        let is_host = room.host.conn == sender_conn;
        let is_ctrl = room
            .controller
            .as_ref()
            .map(|c| c.conn == sender_conn)
            .unwrap_or(false);
        if !is_host && !is_ctrl {
            return None;
        }
        room.last_activity = now;

        let mut sinks = Vec::new();
        if !is_host {
            sinks.push(room.host.out.clone());
        }
        if let Some(c) = room.controller.as_ref() {
            if c.conn != sender_conn {
                sinks.push(c.out.clone());
            }
        }
        Some(RouteTargets { sinks })
    }

    /// Refresh a room's idle clock on a keepalive from one of its members, so an
    /// otherwise-idle ACTIVE session (e.g. after a run finishes) is not idle-swept
    /// while the peer is still present. No-op for an unknown room or a non-member.
    pub fn touch(&mut self, room_id: &str, conn: ConnId, now: Instant) {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return;
        };
        let is_host = room.host.conn == conn;
        let is_ctrl = room
            .controller
            .as_ref()
            .map(|c| c.conn == conn)
            .unwrap_or(false);
        if is_host || is_ctrl {
            room.last_activity = now;
        }
    }

    /// Look up the room id associated with a connection.
    pub fn room_of(&self, conn: ConnId) -> Option<String> {
        self.conn_to_room.get(&conn).cloned()
    }

    /// Whether the given connection is the Host of its room.
    pub fn is_host(&self, conn: ConnId) -> bool {
        self.conn_to_room
            .get(&conn)
            .and_then(|id| self.rooms.get(id))
            .map(|r| r.host.conn == conn)
            .unwrap_or(false)
    }

    /// Close a room and notify the surviving peer(s). Returns the notified
    /// outbound sinks (excluding `originator` if provided) and the previous state.
    ///
    /// Used by revoke (Host→relay, FR-010/AC-13) and by peer disconnects.
    pub fn close_room(&mut self, room_id: &str, originator: Option<ConnId>) -> Vec<Outbound> {
        let room = match self.rooms.get_mut(room_id) {
            Some(r) => r,
            None => return Vec::new(),
        };
        if room.state.is_terminal() {
            // Still surface peers for index cleanup, but nothing to notify.
        }
        room.state = RoomState::Closed;

        let mut notify = Vec::new();
        let host_conn = room.host.conn;
        let pair_code_hash = room.pair_code_hash;
        let resume_token_hash = room.resume_token_hash;
        if Some(host_conn) != originator {
            notify.push(room.host.out.clone());
        }
        let mut ctrl_conn = None;
        if let Some(c) = room.controller.as_ref() {
            ctrl_conn = Some(c.conn);
            if Some(c.conn) != originator {
                notify.push(c.out.clone());
            }
        }

        // Drop indexes for this room — but ONLY where they still point at THIS
        // room. A single Host connection legitimately owns several rooms at once
        // (an ACTIVE session PLUS a re-armed standing rendezvous), and those
        // sibling rooms share BOTH the Host conn id AND the pair_code_hash.
        // Removing blindly by key would de-index a sibling: closing a finished
        // session would drop the standing rendezvous's `pending_by_code` entry, so
        // the phone could never join again (endless `join_invalid` on reconnect).
        if self.conn_to_room.get(&host_conn).map(String::as_str) == Some(room_id) {
            self.conn_to_room.remove(&host_conn);
        }
        if let Some(cc) = ctrl_conn {
            if self.conn_to_room.get(&cc).map(String::as_str) == Some(room_id) {
                self.conn_to_room.remove(&cc);
            }
        }
        if self.pending_by_code.get(&pair_code_hash).map(String::as_str) == Some(room_id) {
            self.pending_by_code.remove(&pair_code_hash);
        }
        if let Some(h) = resume_token_hash {
            if self.resume_by_token.get(&h).map(String::as_str) == Some(room_id) {
                self.resume_by_token.remove(&h);
            }
        }
        self.rooms.remove(room_id);
        notify
    }

    /// Handle a raw connection drop.
    ///
    /// When the **Controller** drops from an ACTIVE room the room is kept alive
    /// for `reconnect_window` (controller-away): the Host is notified with
    /// `peer_left` (so it tears down its live session) but the room and its
    /// resume token survive so a `resume` can re-attach. In every other case
    /// (Host drop, or a drop before the room reached ACTIVE) the room is closed.
    ///
    /// Returns the room id and the survivor sinks to receive `peer_left`.
    pub fn on_disconnect(
        &mut self,
        conn: ConnId,
        now: Instant,
        reconnect_window: std::time::Duration,
    ) -> Option<(String, Vec<Outbound>)> {
        let room_id = self.conn_to_room.get(&conn).cloned()?;
        let room = self.rooms.get_mut(&room_id)?;

        let is_controller = room
            .controller
            .as_ref()
            .map(|c| c.conn == conn)
            .unwrap_or(false);

        // Controller dropped from an established session → keep the room alive so
        // the phone can resume within the window.
        if is_controller && room.state == RoomState::Active {
            let host_out = room.host.out.clone();
            room.controller = None;
            room.resume_deadline = Some(now + reconnect_window);
            self.conn_to_room.remove(&conn);
            return Some((room_id, vec![host_out]));
        }

        // Host dropped, or the Controller dropped mid-handshake → close.
        let survivors = self.close_room(&room_id, Some(conn));
        Some((room_id, survivors))
    }

    /// Sweep expired pending rooms and idle active rooms. Returns closed room ids
    /// and their survivor sinks for `peer_left` notification.
    pub fn sweep(&mut self, now: Instant, idle_timeout: std::time::Duration) -> Vec<(String, Vec<Outbound>)> {
        let mut to_close: Vec<String> = Vec::new();
        for room in self.rooms.values_mut() {
            match room.state {
                RoomState::PendingPair | RoomState::Handshaking if now >= room.expires_at => {
                    // Pending pair code TTL elapsed → EXPIRED / CLOSED.
                    to_close.push(room.room_id.clone());
                }
                // Controller away too long → reconnect window elapsed, close it.
                RoomState::Active
                    if room
                        .resume_deadline
                        .map(|d| now >= d)
                        .unwrap_or(false) =>
                {
                    to_close.push(room.room_id.clone());
                }
                RoomState::Active if now.duration_since(room.last_activity) >= idle_timeout => {
                    to_close.push(room.room_id.clone());
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for id in to_close {
            let survivors = self.close_room(&id, None);
            out.push((id, survivors));
        }
        out
    }

    /// Number of live rooms (for metrics/tests).
    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    /// Current state of a room, if it exists (tests/introspection).
    pub fn state_of(&self, room_id: &str) -> Option<RoomState> {
        self.rooms.get(room_id).map(|r| r.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn sink() -> Outbound {
        let (tx, _rx) = mpsc::unbounded_channel();
        tx
    }

    fn pending(reg: &mut RoomRegistry, code_hash: u64, host: ConnId, now: Instant, ttl: Duration) -> String {
        let room_id = format!("rm_{host}");
        reg.create_pending(room_id.clone(), code_hash, host, sink(), now, now + ttl);
        room_id
    }

    #[test]
    fn join_then_handshake_reaches_active() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = pending(&mut reg, 1, 10, now, Duration::from_secs(60));
        assert_eq!(reg.state_of(&room), Some(RoomState::PendingPair));

        match reg.join(1, 20, sink(), 1001, now) {
            JoinOutcome::Joined { room_id, .. } => assert_eq!(room_id, room),
            _ => panic!("expected Joined"),
        }
        assert_eq!(reg.state_of(&room), Some(RoomState::Handshaking));

        // Only host done → still handshaking.
        assert_eq!(reg.mark_handshake_done(&room, 10, now), Some((room.clone(), false)));
        assert_eq!(reg.state_of(&room), Some(RoomState::Handshaking));
        // Controller done → ACTIVE.
        assert_eq!(reg.mark_handshake_done(&room, 20, now), Some((room.clone(), true)));
        assert_eq!(reg.state_of(&room), Some(RoomState::Active));
    }

    #[test]
    fn handshake_control_routes_while_handshaking_but_data_waits_for_active() {
        // Regression: E2E handshake `control` frames MUST route while the room
        // is HANDSHAKING, otherwise offer/answer never reach the peer and the
        // room can never become ACTIVE (deadlock).
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = pending(&mut reg, 7, 10, now, Duration::from_secs(60));
        assert!(matches!(reg.join(7, 20, sink(), 1007, now), JoinOutcome::Joined { .. }));
        assert_eq!(reg.state_of(&room), Some(RoomState::Handshaking));

        // Handshake control frame (allow_handshaking = true) → routed to peer.
        assert!(reg.route(&room, 10, now, true).is_some(), "control must route while handshaking");
        // A data frame (allow_handshaking = false) → refused until ACTIVE.
        assert!(reg.route(&room, 10, now, false).is_none(), "data must wait for ACTIVE");

        // Complete handshake → ACTIVE, then data frames route too.
        reg.mark_handshake_done(&room, 10, now);
        reg.mark_handshake_done(&room, 20, now);
        assert_eq!(reg.state_of(&room), Some(RoomState::Active));
        assert!(reg.route(&room, 10, now, false).is_some(), "data routes once ACTIVE");
    }

    #[test]
    fn handshake_done_routed_by_room_id_not_latest_conn() {
        // Regression: a single Host connection legitimately belongs to two rooms
        // at once — an in-progress handshake AND a freshly re-armed durable
        // standing rendezvous. `conn_to_room[host]` points at the latter, so
        // `handshake_done` MUST be resolved by the frame's room_id; otherwise the
        // real session never reaches ACTIVE (durable reconnect stuck at
        // "Connected — waiting for data…").
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let host: ConnId = 10;

        // Room A: controller joined → HANDSHAKING.
        let room_a = pending(&mut reg, 50, host, now, Duration::from_secs(60));
        assert!(matches!(reg.join(50, 20, sink(), 5000, now), JoinOutcome::Joined { .. }));

        // Host re-arms a standing rendezvous (Room B) on the SAME conn; this
        // overwrites conn_to_room[host] → B.
        let room_b = String::from("rm_10_standby");
        reg.create_pending(room_b.clone(), 51, host, sink(), now, now + Duration::from_secs(60));
        assert_eq!(reg.room_of(host), Some(room_b.clone()), "conn_to_room now points at B");

        // Completing Room A's handshake by room_id must mark A (not B).
        reg.mark_handshake_done(&room_a, 20, now);
        reg.mark_handshake_done(&room_a, host, now);
        assert_eq!(
            reg.state_of(&room_a),
            Some(RoomState::Active),
            "room A reaches ACTIVE despite conn_to_room pointing at B",
        );
        assert_eq!(
            reg.state_of(&room_b),
            Some(RoomState::PendingPair),
            "the standing rendezvous B is untouched",
        );
    }

    #[test]
    fn re_arming_standing_rendezvous_supersedes_old_pending_no_leak() {
        // A host re-announcing the same pair code (durable-reconnect re-arm after
        // every controller (re)connect) must NOT accumulate orphaned persistent
        // pending rooms. Each re-arm supersedes the previous PENDING rendezvous.
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let host: ConnId = 10;
        let ttl = Duration::from_secs(3650 * 24 * 3600); // persistent-ish

        reg.create_pending("rm_a".into(), 99, host, sink(), now, now + ttl);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.room_of(host), Some("rm_a".into()));

        // Re-arm the SAME code → old pending room is closed, not leaked.
        reg.create_pending("rm_b".into(), 99, host, sink(), now, now + ttl);
        assert_eq!(reg.len(), 1, "old standing rendezvous superseded, not leaked");
        assert_eq!(reg.state_of("rm_a"), None, "previous pending room removed");
        assert_eq!(reg.state_of("rm_b"), Some(RoomState::PendingPair));
        assert_eq!(reg.room_of(host), Some("rm_b".into()));

        // Many re-arms stay bounded at one live room.
        for i in 0..50 {
            reg.create_pending(format!("rm_{i}"), 99, host, sink(), now, now + ttl);
        }
        assert_eq!(reg.len(), 1, "repeated re-arms never accumulate");
    }

    #[test]
    fn re_arm_does_not_touch_active_session_on_same_host() {
        // The supersede is keyed on `pending_by_code`, which an ACTIVE session is
        // removed from on join — so re-arming the standing rendezvous must leave a
        // live session on the SAME host connection untouched.
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let host: ConnId = 10;

        // Room A: controller joined + handshaken → ACTIVE (code 60 consumed).
        let room_a = active_room(&mut reg, 60, host, 20, 6000, now);
        assert_eq!(reg.state_of(&room_a), Some(RoomState::Active));

        // Host arms a standing rendezvous on a DIFFERENT code, then re-arms it.
        reg.create_pending("rm_standby1".into(), 61, host, sink(), now, now + Duration::from_secs(60));
        reg.create_pending("rm_standby2".into(), 61, host, sink(), now, now + Duration::from_secs(60));

        assert_eq!(reg.state_of(&room_a), Some(RoomState::Active), "active session untouched");
        assert_eq!(reg.state_of("rm_standby1"), None, "old standby superseded");
        assert_eq!(reg.state_of("rm_standby2"), Some(RoomState::PendingPair));
        // A resume for the active session still works (its token index is intact).
        reg.on_disconnect(20, now, Duration::from_secs(60));
        assert!(matches!(
            reg.resume(6000, 6001, 21, sink(), now + Duration::from_secs(1)),
            ResumeOutcome::Resumed { .. }
        ));
    }

    #[test]
    fn closing_active_session_keeps_sibling_standing_rendezvous_joinable() {
        // Regression: an ACTIVE session and the host's re-armed standing
        // rendezvous share the same pair_code_hash AND host conn. Closing the
        // finished session must NOT de-index the standing rendezvous, otherwise a
        // reconnecting phone gets endless `join_invalid` and can never re-attach.
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let host: ConnId = 10;
        let code = 77u64;

        // Session on `code` reaches ACTIVE (join consumes the code from the index).
        let room_a = active_room(&mut reg, code, host, 20, 7000, now);
        // Host re-arms a standing rendezvous on the SAME code (durable reconnect).
        reg.create_pending(
            "rm_standby".into(), code, host, sink(), now,
            now + Duration::from_secs(3650 * 24 * 3600),
        );
        assert_eq!(reg.room_of(host), Some("rm_standby".into()));

        // The finished session is closed (revoke / idle / host tears it down).
        reg.close_room(&room_a, None);

        // The standing rendezvous survives and is still joinable — the reconnect
        // path works.
        assert_eq!(reg.state_of("rm_standby"), Some(RoomState::PendingPair));
        assert_eq!(reg.room_of(host), Some("rm_standby".into()), "host index intact");
        match reg.join(code, 30, sink(), 7001, now) {
            JoinOutcome::Joined { room_id, .. } => assert_eq!(room_id, "rm_standby"),
            _ => panic!("standing rendezvous must remain joinable after sibling close"),
        }
    }

    #[test]
    fn expired_pair_join_is_invalid_and_no_room() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let _room = pending(&mut reg, 2, 10, now, Duration::from_millis(1));
        let later = now + Duration::from_secs(1);
        assert!(matches!(reg.join(2, 20, sink(), 1002, later), JoinOutcome::Invalid));
    }

    #[test]
    fn second_controller_is_full() {
        // Directly exercise MAX_CONTROLLER without pair-code consumption by
        // re-registering the same code hash after a successful join is not
        // possible; instead verify Full when a controller already exists.
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = pending(&mut reg, 3, 10, now, Duration::from_secs(60));
        assert!(matches!(reg.join(3, 20, sink(), 1003, now), JoinOutcome::Joined { .. }));

        // Simulate a concurrent race where the code is briefly re-indexed while
        // the room is still pending but already has a controller; a second join
        // must be refused as Full (MAX_CONTROLLER=1), never admitted.
        reg.pending_by_code.insert(3, room.clone());
        if let Some(r) = reg.rooms.get_mut(&room) {
            r.state = RoomState::PendingPair;
        }
        assert!(matches!(reg.join(3, 30, sink(), 1030, now), JoinOutcome::Full));
    }

    #[test]
    fn revoke_closes_and_clears_indexes() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = pending(&mut reg, 4, 10, now, Duration::from_secs(60));
        let _ = reg.join(4, 20, sink(), 1004, now);
        let survivors = reg.close_room(&room, Some(10));
        assert_eq!(survivors.len(), 1, "controller should be notified");
        assert_eq!(reg.state_of(&room), None, "room removed after close");
        assert!(reg.room_of(10).is_none());
        assert!(reg.room_of(20).is_none());
    }

    /// Drive a fresh room all the way to ACTIVE with a known resume token hash.
    fn active_room(reg: &mut RoomRegistry, code: u64, host: ConnId, ctrl: ConnId, token: u64, now: Instant) -> String {
        let room = pending(reg, code, host, now, Duration::from_secs(60));
        assert!(matches!(reg.join(code, ctrl, sink(), token, now), JoinOutcome::Joined { .. }));
        reg.mark_handshake_done(&room, host, now);
        reg.mark_handshake_done(&room, ctrl, now);
        assert_eq!(reg.state_of(&room), Some(RoomState::Active));
        room
    }

    #[test]
    fn controller_drop_keeps_room_and_resume_reattaches() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = active_room(&mut reg, 40, 10, 20, 5000, now);

        // Controller drops → room stays ACTIVE (away), host is a survivor.
        let (rid, survivors) = reg
            .on_disconnect(20, now, Duration::from_secs(60))
            .expect("host must be notified");
        assert_eq!(rid, room);
        assert_eq!(survivors.len(), 1, "host is notified peer_left");
        assert_eq!(reg.state_of(&room), Some(RoomState::Active), "room kept alive");
        assert!(reg.room_of(20).is_none(), "old controller conn index cleared");

        // Resume with the current token → back to HANDSHAKING with a new token.
        match reg.resume(5000, 6000, 21, sink(), now + Duration::from_secs(5)) {
            ResumeOutcome::Resumed { room_id, .. } => assert_eq!(room_id, room),
            _ => panic!("expected Resumed"),
        }
        assert_eq!(reg.state_of(&room), Some(RoomState::Handshaking));
        assert_eq!(reg.room_of(21), Some(room.clone()));

        // The redeemed token can never be replayed.
        assert!(matches!(reg.resume(5000, 7000, 22, sink(), now), ResumeOutcome::Invalid));

        // Re-handshake → ACTIVE again.
        reg.mark_handshake_done(&room, 10, now);
        reg.mark_handshake_done(&room, 21, now);
        assert_eq!(reg.state_of(&room), Some(RoomState::Active));
    }

    #[test]
    fn resume_after_window_is_invalid_and_swept() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = active_room(&mut reg, 41, 10, 20, 5100, now);
        reg.on_disconnect(20, now, Duration::from_secs(60));

        // Past the window → resume refused …
        let later = now + Duration::from_secs(61);
        assert!(matches!(reg.resume(5100, 5101, 21, sink(), later), ResumeOutcome::Invalid));
        // … and the sweeper closes the abandoned room.
        let closed = reg.sweep(later, Duration::from_secs(3600));
        assert_eq!(closed.len(), 1);
        assert_eq!(reg.state_of(&room), None);
    }

    #[test]
    fn host_drop_closes_room_no_resume() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = active_room(&mut reg, 42, 10, 20, 5200, now);
        let (_rid, survivors) = reg
            .on_disconnect(10, now, Duration::from_secs(60))
            .expect("controller notified");
        assert_eq!(survivors.len(), 1, "controller is notified peer_left");
        assert_eq!(reg.state_of(&room), None, "room closed when host drops");
        assert!(matches!(reg.resume(5200, 5201, 21, sink(), now), ResumeOutcome::Invalid));
    }

    #[test]
    fn touch_refreshes_idle_clock_and_prevents_sweep() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let room = active_room(&mut reg, 43, 10, 20, 5300, now);
        let idle = Duration::from_secs(60);

        // Just before the idle deadline, a keepalive from the controller refreshes
        // the clock so the sweep leaves the room alone.
        let almost = now + Duration::from_secs(59);
        reg.touch(&room, 20, almost);
        assert!(reg.sweep(now + Duration::from_secs(90), idle).is_empty());
        assert_eq!(reg.state_of(&room), Some(RoomState::Active), "kept alive by touch");

        // A non-member touch is a no-op; without further keepalives the room is
        // eventually swept once idle elapses past the last touch.
        reg.touch(&room, 999, almost + Duration::from_secs(1));
        let closed = reg.sweep(almost + idle + Duration::from_secs(1), idle);
        assert_eq!(closed.len(), 1, "idle room swept after keepalives stop");
        assert_eq!(reg.state_of(&room), None);
    }

    #[test]
    fn sweep_expires_pending_rooms() {
        let mut reg = RoomRegistry::default();
        let now = Instant::now();
        let _room = pending(&mut reg, 5, 10, now, Duration::from_millis(1));
        let later = now + Duration::from_secs(2);
        let closed = reg.sweep(later, Duration::from_secs(3600));
        assert_eq!(closed.len(), 1);
        assert!(reg.is_empty());
    }
}
