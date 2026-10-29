/**
 * Bookkeeping of the remote-desktop IPC client (./remote.ts), free of Vue,
 * Tauri and the transport so it can be unit-tested in plain Node.
 *
 * - `invoke` calls get an id and wait in a pending map until the matching
 *   `ipc_result`. While the link is down they are queued and sent, in order,
 *   once it is (re)authenticated. Each call times out after `timeoutMs`
 *   (counted from the `invoke`, queue time included).
 * - Calls already sent when the link drops are rejected: the controlled side
 *   forgets them on detach, so their results will never come.
 * - `listen` subscriptions are refcounted per event name: only the first
 *   subscriber sends `ipc_listen`, only the last unlisten sends `ipc_unlisten`,
 *   and every live event is re-subscribed after each (re)authentication.
 */

/** The `ipc_*` command frames the core asks the transport to seal and send. */
export type IpcFrame =
  | { kind: 'cmd'; action: 'ipc_invoke'; id: string; cmd: string; args: unknown }
  | { kind: 'cmd'; action: 'ipc_listen'; event: string }
  | { kind: 'cmd'; action: 'ipc_unlisten'; event: string }

/** A Tauri-like event object handed to `listen` handlers. */
export interface IpcEvent<T> {
  event: string
  id: number
  payload: T
}

export type IpcEventHandler<T> = (event: IpcEvent<T>) => void

export interface IpcCoreDeps {
  /** Seal and send one frame; false when the link cannot carry it right now. */
  send: (frame: IpcFrame) => boolean
  /** A fresh correlation id per call. */
  newId: () => string
  /** Per-call timeout (ms). */
  timeoutMs?: number
  setTimer?: (fn: () => void, ms: number) => unknown
  clearTimer?: (handle: unknown) => void
  /** Where a throwing event handler is reported (it must not break the others). */
  onHandlerError?: (err: unknown) => void
}

export const IPC_TIMEOUT_MS = 60_000
export const ERR_TIMEOUT = 'remote_timeout'
export const ERR_CONNECTION_LOST = 'remote_connection_lost'
export const ERR_CLOSED = 'remote_closed'

interface PendingCall {
  frame: IpcFrame
  /** Sent on the current link (else still queued). */
  sent: boolean
  resolve: (value: unknown) => void
  reject: (reason: unknown) => void
  timer: unknown
}

export class IpcCore {
  private readonly deps: Required<IpcCoreDeps>
  /** Insertion order = invoke order, which is also the flush order. */
  private readonly pending = new Map<string, PendingCall>()
  private readonly subs = new Map<string, Set<IpcEventHandler<unknown>>>()
  private active = false
  private closed = false

  constructor(deps: IpcCoreDeps) {
    this.deps = {
      timeoutMs: IPC_TIMEOUT_MS,
      setTimer: (fn, ms) => setTimeout(fn, ms),
      clearTimer: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
      onHandlerError: (err) => console.error('[remote-ipc] event handler failed', err),
      ...deps,
    }
  }

  /** Calls waiting for a result (sent or queued). */
  get pendingCount(): number {
    return this.pending.size
  }

  /** Event names with at least one live subscriber. */
  get events(): string[] {
    return [...this.subs.keys()]
  }

  invoke<T>(cmd: string, args?: unknown): Promise<T> {
    if (this.closed) return Promise.reject(new Error(ERR_CLOSED))
    const id = this.deps.newId()
    return new Promise<T>((resolve, reject) => {
      const call: PendingCall = {
        frame: { kind: 'cmd', action: 'ipc_invoke', id, cmd, args: args ?? {} },
        sent: false,
        resolve: resolve as (value: unknown) => void,
        reject,
        timer: this.deps.setTimer(() => {
          if (this.pending.get(id) !== call) return
          this.pending.delete(id)
          reject(new Error(ERR_TIMEOUT))
        }, this.deps.timeoutMs),
      }
      this.pending.set(id, call)
      if (this.active) call.sent = this.deps.send(call.frame)
    })
  }

  /** Subscribe; returns the unlisten function (idempotent). */
  listen<T>(event: string, handler: IpcEventHandler<T>): () => void {
    if (this.closed) return () => {}
    const h = handler as IpcEventHandler<unknown>
    let set = this.subs.get(event)
    if (!set) {
      set = new Set()
      this.subs.set(event, set)
      if (this.active) this.deps.send({ kind: 'cmd', action: 'ipc_listen', event })
    }
    // A wrapper per call, so the same handler listened twice unlistens once each.
    const entry: IpcEventHandler<unknown> = (e) => h(e)
    set.add(entry)
    let done = false
    return () => {
      if (done) return
      done = true
      const cur = this.subs.get(event)
      if (!cur || !cur.delete(entry) || cur.size > 0) return
      this.subs.delete(event)
      if (this.active && !this.closed) this.deps.send({ kind: 'cmd', action: 'ipc_unlisten', event })
    }
  }

  /** The link is (re)authenticated: re-subscribe, then flush queued calls. */
  linkUp(): void {
    if (this.closed) return
    this.active = true
    for (const event of this.subs.keys()) {
      if (!this.deps.send({ kind: 'cmd', action: 'ipc_listen', event })) {
        // Lost again mid-flush; the next linkUp starts over.
        this.active = false
        return
      }
    }
    for (const call of this.pending.values()) {
      if (call.sent) continue
      call.sent = this.deps.send(call.frame)
      if (!call.sent) {
        this.active = false
        return
      }
    }
  }

  /** The link dropped: fail what was in flight, keep the queue. */
  linkDown(): void {
    this.active = false
    for (const [id, call] of [...this.pending]) {
      if (!call.sent) continue
      this.settle(id, call)
      call.reject(new Error(ERR_CONNECTION_LOST))
    }
  }

  /** Correlate an `ipc_result`. Unknown / late ids are ignored. */
  handleResult(id: string, ok: boolean, value: unknown, error: unknown): void {
    const call = this.pending.get(id)
    if (!call) return
    this.settle(id, call)
    if (ok) call.resolve(value)
    else call.reject(error)
  }

  /** Dispatch an `ipc_event` to every current subscriber of `event`. */
  handleEvent(event: string, payload: unknown): void {
    const set = this.subs.get(event)
    if (!set) return
    const e: IpcEvent<unknown> = { event, id: 0, payload }
    for (const h of [...set]) {
      try {
        h(e)
      } catch (err) {
        this.deps.onHandlerError(err)
      }
    }
  }

  /** For good: reject everything and drop all subscriptions. */
  close(): void {
    if (this.closed) return
    this.closed = true
    this.active = false
    this.subs.clear()
    for (const [id, call] of [...this.pending]) {
      this.settle(id, call)
      call.reject(new Error(ERR_CLOSED))
    }
  }

  private settle(id: string, call: PendingCall): void {
    this.pending.delete(id)
    this.deps.clearTimer(call.timer)
  }
}
