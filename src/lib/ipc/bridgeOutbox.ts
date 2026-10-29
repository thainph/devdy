/**
 * The remote-desktop bridge's way back to the Rust side (src/bridge/main.ts),
 * free of Tauri so it can be unit-tested in plain Node.
 *
 * Results and events must reach the Controller in the order the bridge saw
 * them (a `run://status` event must not overtake the result that started the
 * run). Separate concurrent invokes would race on Tauri's multi-threaded async
 * runtime, so every item goes through this one queue and is delivered in
 * batches, ONE batch in flight at a time: Rust handles a batch in order before
 * the next one can arrive.
 */

/** One item of a `remote_bridge_batch` call (see `src-tauri/src/remote/ipc.rs`). */
export type BridgeItem =
  | { kind: 'result'; id: string; ok: boolean; value: unknown; error: unknown }
  | { kind: 'event'; event: string; payload: unknown }

export interface BridgeOutboxDeps {
  /** Deliver one batch; the next is not sent before this settles. */
  sendBatch: (items: BridgeItem[]) => Promise<void>
  /** A batch could not be delivered (its items are gone). */
  onError?: (err: unknown) => void
}

export class BridgeOutbox {
  private readonly deps: BridgeOutboxDeps
  private queue: BridgeItem[] = []
  private flushing = false

  constructor(deps: BridgeOutboxDeps) {
    this.deps = deps
  }

  /** Items waiting for the next batch. */
  get size(): number {
    return this.queue.length
  }

  push(item: BridgeItem): void {
    this.queue.push(item)
    if (!this.flushing) void this.flush()
  }

  private async flush(): Promise<void> {
    this.flushing = true
    try {
      while (this.queue.length > 0) {
        const batch = this.queue
        this.queue = []
        try {
          await this.deps.sendBatch(batch)
        } catch (e) {
          this.deps.onError?.(e)
        }
      }
    } finally {
      this.flushing = false
    }
  }
}
