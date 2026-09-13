//! Prioritised outbound queue from the Host to the relay.
//!
//! Every frame the Host sends — live stream, output lines, permission requests,
//! command acks, control frames — funnels through one channel into the socket
//! writer task. That channel used to be unbounded, which meant a Controller on a
//! slow link (phone on 4G) plus a chatty run could grow the queue without limit:
//! the Host buffers megabytes it can never flush, and the Controller eventually
//! receives a long tail of stale output.
//!
//! [`OutboundTx`] keeps the channel unbounded — so a send is still infallible
//! and non-blocking at every call site — but tracks queue depth and lets the
//! PRODUCER shed load once the link is visibly congested. Shedding is
//! priority-aware and never touches a frame that carries meaning:
//!
//! - [`OutboundTx::send`] — critical. Permission requests, acks, auth results,
//!   history, notices and relay control frames. Always queued.
//! - [`OutboundTx::send_lossy`] — sheddable. Live stream events and console
//!   output, which the Controller can always backfill with `request_history`
//!   (FR-005). Dropped above [`OUTBOUND_HIGH_WATER`].
//!
//! Dropping live output rather than queueing it unboundedly is the right trade:
//! the Controller stays responsive to permission prompts (the one interaction
//! that blocks a run) instead of drowning behind a backlog of log lines.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::remote::protocol::Envelope;

/// Queue depth above which sheddable frames are dropped.
///
/// Sized for roughly a second of a very chatty run. A healthy link drains far
/// below this; sustained depth above it means the socket cannot keep up, and no
/// amount of buffering will fix that.
pub const OUTBOUND_HIGH_WATER: usize = 1024;

/// Sender half: cloneable, non-blocking, depth-aware.
#[derive(Clone)]
pub struct OutboundTx {
    tx: mpsc::UnboundedSender<Envelope>,
    depth: Arc<AtomicUsize>,
    shed: Arc<AtomicU64>,
}

/// Receiver half, owned by the socket writer task.
pub struct OutboundRx {
    rx: mpsc::UnboundedReceiver<Envelope>,
    depth: Arc<AtomicUsize>,
}

/// Create a connected pair.
pub fn channel() -> (OutboundTx, OutboundRx) {
    let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
    let depth = Arc::new(AtomicUsize::new(0));
    (
        OutboundTx {
            tx,
            depth: depth.clone(),
            shed: Arc::new(AtomicU64::new(0)),
        },
        OutboundRx { rx, depth },
    )
}

impl OutboundTx {
    /// Queue a frame that must not be dropped.
    ///
    /// `Err(())` means the writer task is gone (socket closed) — callers treat
    /// that as "the room is gone" and stop.
    pub fn send(&self, env: Envelope) -> Result<(), ()> {
        match self.tx.send(env) {
            Ok(()) => {
                self.depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => Err(()),
        }
    }

    /// Queue a frame that may be dropped when the link is congested.
    ///
    /// Returns `Err(())` only when the writer is gone — a SHED frame is a normal
    /// outcome and reports `Ok(())`, because the caller has no useful recovery
    /// beyond carrying on.
    pub fn send_lossy(&self, env: Envelope) -> Result<(), ()> {
        if self.tx.is_closed() {
            return Err(());
        }
        if self.depth.load(Ordering::Relaxed) >= OUTBOUND_HIGH_WATER {
            let n = self.shed.fetch_add(1, Ordering::Relaxed) + 1;
            // Log on the first drop and then once per 1000, so a congested link
            // leaves a trace without flooding the log itself.
            if n == 1 || n % 1000 == 0 {
                tracing::warn!(
                    event = "remote_outbound_shed",
                    dropped_total = n,
                    depth = self.depth.load(Ordering::Relaxed),
                );
            }
            return Ok(());
        }
        self.send(env)
    }

    /// Current queue depth (frames enqueued but not yet written to the socket).
    /// Exercised by the unit tests; kept as part of the gauge's API surface.
    #[allow(dead_code)]
    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }

    /// Total sheddable frames dropped on this connection. Exercised by the unit
    /// tests; the running agent reports it through the shed warning instead.
    #[allow(dead_code)]
    pub fn shed_count(&self) -> u64 {
        self.shed.load(Ordering::Relaxed)
    }
}

impl OutboundRx {
    /// Await the next frame, keeping the depth gauge in step.
    pub async fn recv(&mut self) -> Option<Envelope> {
        let env = self.rx.recv().await?;
        self.depth.fetch_sub(1, Ordering::Relaxed);
        Some(env)
    }

    /// Take an already-queued frame without awaiting. `Err(())` when the queue is
    /// empty (or closed and drained).
    #[allow(dead_code)]
    pub fn try_recv(&mut self) -> Result<Envelope, ()> {
        match self.rx.try_recv() {
            Ok(env) => {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                Ok(env)
            }
            Err(_) => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::protocol::FrameType;

    fn frame(seq: u64) -> Envelope {
        Envelope::data(FrameType::Stream, "rm_1", "Y2lwaGVy".to_string(), seq)
    }

    #[tokio::test]
    async fn critical_frames_are_never_shed() {
        let (tx, mut rx) = channel();
        for i in 0..(OUTBOUND_HIGH_WATER as u64 + 50) {
            tx.send(frame(i)).expect("writer alive");
        }
        assert_eq!(tx.shed_count(), 0, "send() must never drop");
        assert_eq!(tx.depth(), OUTBOUND_HIGH_WATER + 50);
        // Draining brings the gauge back down.
        for _ in 0..(OUTBOUND_HIGH_WATER + 50) {
            rx.recv().await.expect("frame queued");
        }
        assert_eq!(tx.depth(), 0);
    }

    #[tokio::test]
    async fn lossy_frames_are_shed_above_the_high_water_mark() {
        let (tx, mut rx) = channel();
        for i in 0..OUTBOUND_HIGH_WATER as u64 {
            tx.send_lossy(frame(i)).expect("writer alive");
        }
        assert_eq!(tx.shed_count(), 0, "under the mark nothing is dropped");

        tx.send_lossy(frame(9_000)).expect("writer alive");
        assert_eq!(tx.shed_count(), 1);
        assert_eq!(tx.depth(), OUTBOUND_HIGH_WATER, "shed frame never queued");

        // Once the writer drains below the mark, lossy sends resume.
        rx.recv().await.expect("frame queued");
        tx.send_lossy(frame(9_001)).expect("writer alive");
        assert_eq!(tx.shed_count(), 1, "no further drops once drained");
        assert_eq!(tx.depth(), OUTBOUND_HIGH_WATER);
    }

    #[tokio::test]
    async fn both_paths_report_a_closed_writer() {
        let (tx, rx) = channel();
        drop(rx);
        assert!(tx.send(frame(1)).is_err());
        assert!(tx.send_lossy(frame(2)).is_err());
    }
}
