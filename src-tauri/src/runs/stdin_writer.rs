//! Per-run sidecar input: a single task owns the sidecar's stdin and writes
//! queued NDJSON lines one at a time, each fully, in order.
//!
//! Why a task instead of writing from the caller: a write blocks once the pipe
//! is full (the sidecar stopped reading). Cancelling such a write on a timeout
//! can leave half a JSON line in the pipe, and closing stdin to recover would
//! end the session (the codex sidecar treats EOF as "shut down"). So nothing is
//! ever cancelled or closed because of a timeout here:
//! * callers enqueue and wait for an ack with a timeout; on timeout they get an
//!   error, but their line stays queued and is written later, in order;
//! * a write stuck for long raises a notice (via the stall hook) and clears it
//!   once it completes;
//! * the input closes only explicitly — when every `SidecarInput` handle is
//!   dropped (end of turn, cancel, stop) — and only after the queue is drained.

use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// How long a caller waits for its line to be written before giving up waiting.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(30);
/// A single write blocked this long raises the "not reading its input" notice.
pub const STALL_NOTICE_AFTER: Duration = Duration::from_secs(30);

/// Called with `true` when a write has been blocked for `STALL_NOTICE_AFTER`,
/// and with `false` once that write completed.
pub type StallHook = Box<dyn Fn(bool) + Send + Sync>;

struct WriteReq {
    line: String,
    ack: oneshot::Sender<Result<(), String>>,
}

/// Handle to a run's input writer. Cheap to clone; the input closes (EOF to
/// the sidecar, after all queued lines) once the last handle is dropped.
#[derive(Clone)]
pub struct SidecarInput {
    tx: mpsc::UnboundedSender<WriteReq>,
}

impl SidecarInput {
    pub fn spawn<W>(pipe: W, on_stall: StallHook) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::spawn_with(pipe, on_stall, STALL_NOTICE_AFTER)
    }

    fn spawn_with<W>(mut pipe: W, on_stall: StallHook, stall_after: Duration) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<WriteReq>();
        tokio::spawn(async move {
            // Once a write fails the pipe is dead (the sidecar exited); later
            // lines fail fast with the same error.
            let mut broken: Option<String> = None;
            while let Some(req) = rx.recv().await {
                if let Some(err) = &broken {
                    let _ = req.ack.send(Err(err.clone()));
                    continue;
                }
                let write = async {
                    pipe.write_all(req.line.as_bytes()).await?;
                    pipe.flush().await
                };
                tokio::pin!(write);
                let mut stalled = false;
                let res = loop {
                    tokio::select! {
                        r = &mut write => break r,
                        _ = tokio::time::sleep(stall_after), if !stalled => {
                            stalled = true;
                            on_stall(true);
                        }
                    }
                };
                if stalled {
                    on_stall(false);
                }
                let res = res.map_err(|e| format!("write stdin: {}", e));
                if let Err(e) = &res {
                    broken = Some(e.clone());
                }
                let _ = req.ack.send(res);
            }
            // Every handle dropped: the queue is drained, close the pipe (EOF).
            drop(pipe);
        });
        Self { tx }
    }

    /// Queue `line` without waiting for it to be written (fire-and-forget; a
    /// stuck write still raises the stall notice). Errors only if the input is
    /// already closed.
    pub fn queue(&self, line: String) -> Result<(), String> {
        let (ack, _) = oneshot::channel();
        self.tx
            .send(WriteReq { line, ack })
            .map_err(|_| "run is not accepting input".to_string())
    }

    /// Queue `line` and wait (up to `ack_timeout`) until it has been written.
    /// On timeout the line stays queued and will still be delivered in order.
    pub async fn send(&self, line: String, ack_timeout: Duration) -> Result<(), String> {
        let (ack, done) = oneshot::channel();
        self.tx
            .send(WriteReq { line, ack })
            .map_err(|_| "run is not accepting input".to_string())?;
        match tokio::time::timeout(ack_timeout, done).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("run is not accepting input".to_string()),
            Err(_) => Err(
                "The session is not reading its input right now. This message is queued and will be delivered in order once it does — don't send it again."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncReadExt;

    fn recorder() -> (StallHook, Arc<Mutex<Vec<bool>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        (Box::new(move |v| s.lock().unwrap().push(v)), seen)
    }

    // The sidecar doesn't read for a while: the caller times out, but nothing is
    // closed or cut — a later line is queued behind it, and once the sidecar
    // reads, it receives both lines whole and in order, then EOF on close.
    #[tokio::test]
    async fn ack_timeout_keeps_input_open_ordered_and_whole() {
        // A small pipe so a large line blocks until the reader starts.
        let (writer, mut reader) = tokio::io::duplex(1024);
        let (hook, stalls) = recorder();
        let input = SidecarInput::spawn_with(writer, hook, Duration::from_millis(100));

        let big = format!(
            "{{\"type\":\"prompt\",\"text\":\"{}\"}}\n",
            "a".repeat(200_000)
        );
        let err = input
            .send(big.clone(), Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(err.contains("queued"), "{err}");

        // Queued behind the stuck write; must not overtake or interleave it.
        let input2 = input.clone();
        let second = tokio::spawn(async move {
            input2
                .send("{\"type\":\"end\"}\n".to_string(), Duration::from_secs(5))
                .await
        });

        // The sidecar starts reading again.
        let mut got = Vec::new();
        let read = tokio::spawn(async move {
            reader.read_to_end(&mut got).await.unwrap();
            got
        });
        assert!(second.await.unwrap().is_ok());
        assert_eq!(*stalls.lock().unwrap(), vec![true, false]);

        // Explicit close: drop every handle → EOF after the queue drained.
        drop(input);
        let got = tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .expect("EOF after close")
            .unwrap();
        let expected = format!("{}{{\"type\":\"end\"}}\n", big);
        assert_eq!(String::from_utf8(got).unwrap(), expected);
    }

    // Fast path: no stall notice, immediate ack.
    #[tokio::test]
    async fn healthy_write_acks_without_notice() {
        let (writer, mut reader) = tokio::io::duplex(64 * 1024);
        let (hook, stalls) = recorder();
        let input = SidecarInput::spawn_with(writer, hook, Duration::from_millis(100));
        input
            .send("{\"x\":1}\n".to_string(), Duration::from_secs(1))
            .await
            .unwrap();
        drop(input);
        let mut got = String::new();
        reader.read_to_string(&mut got).await.unwrap();
        assert_eq!(got, "{\"x\":1}\n");
        assert!(stalls.lock().unwrap().is_empty());
    }

    // The sidecar is gone (pipe closed): the error is reported, and later lines
    // fail fast instead of hanging.
    #[tokio::test]
    async fn dead_pipe_fails_fast() {
        let (writer, reader) = tokio::io::duplex(64);
        drop(reader);
        let (hook, _) = recorder();
        let input = SidecarInput::spawn_with(writer, hook, Duration::from_secs(5));
        assert!(input
            .send("a\n".to_string(), Duration::from_secs(1))
            .await
            .is_err());
        assert!(input
            .send("b\n".to_string(), Duration::from_secs(1))
            .await
            .is_err());
    }
}
