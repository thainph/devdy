//! Event tap at the source (FR-004 "ưu tiên tap tại nguồn").
//!
//! Rather than re-subscribing to dynamically-named Tauri events
//! (`run:event:<id>` …) from the forwarder, `drain_sidecar` publishes every run
//! event onto this in-process broadcast bus as it emits to the frontend. The
//! Remote Control forwarder (when a room is ACTIVE) subscribes and seals each
//! event for the Controller. When no forwarder is listening the send is a
//! no-op — a `broadcast::Sender` with no receivers simply drops the value — so
//! there is ZERO cost on the local run path when remote is disabled.

use serde_json::Value;
use tokio::sync::broadcast;

/// One run event tapped at `drain_sidecar`. Mirrors the three frontend channels.
#[derive(Debug, Clone)]
pub enum RemoteRunEvent {
    /// Raw stream-json SDK message (was `run:event:<id>`).
    Event { run_id: String, event: Value },
    /// A console/output line (was `run:output:<id>`).
    Output {
        run_id: String,
        line: String,
        is_stderr: bool,
    },
    /// A tool permission request (was `run:permission_request:<id>`). Carries the
    /// full context the Controller needs to decide (BR-007).
    PermissionRequest {
        run_id: String,
        request_id: String,
        tool: String,
        input: Value,
        cwd: Option<String>,
    },
    /// The run finished (was `run:done:<id>`).
    Done { run_id: String, status: String },
    /// The run's engine/model/permission-mode selection changed (was
    /// `run:meta:<id>`). Published whenever the desktop composer OR a remote
    /// controller updates a selector, so both surfaces stay in lock-step.
    Meta {
        run_id: String,
        engine: Option<String>,
        model: Option<String>,
        permission_mode: Option<String>,
    },
}

impl RemoteRunEvent {
    /// The run this event belongs to (used to gate on the share allow-list).
    pub fn run_id(&self) -> &str {
        match self {
            RemoteRunEvent::Event { run_id, .. }
            | RemoteRunEvent::Output { run_id, .. }
            | RemoteRunEvent::PermissionRequest { run_id, .. }
            | RemoteRunEvent::Done { run_id, .. }
            | RemoteRunEvent::Meta { run_id, .. } => run_id,
        }
    }
}

/// The shared broadcast channel. Held in Tauri managed state; cloned into the
/// forwarder as a subscriber. Capacity is generous so a briefly-slow forwarder
/// lags rather than drops the newest live events (a lagging receiver is told via
/// `RecvError::Lagged` and can trigger a history replay — FR-005/FR-012).
#[derive(Clone)]
pub struct RemoteBus {
    tx: broadcast::Sender<RemoteRunEvent>,
}

impl RemoteBus {
    /// Create a bus with a bounded ring buffer.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        RemoteBus { tx }
    }

    /// Publish an event. No-op when there are no subscribers (remote disabled).
    pub fn publish(&self, event: RemoteRunEvent) {
        // Ignore the "no receivers" error — that is the normal disabled state.
        let _ = self.tx.send(event);
    }

    /// Subscribe to the live stream (called by the forwarder when a room opens).
    pub fn subscribe(&self) -> broadcast::Receiver<RemoteRunEvent> {
        self.tx.subscribe()
    }
}

impl Default for RemoteBus {
    fn default() -> Self {
        Self::new()
    }
}
