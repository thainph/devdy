/**
 * One relay connection for every remote-desktop window of this machine.
 *
 * The controlled Devdy accepts ONE controller per session (the newest join
 * wins), so pop-outs opened from a remote-desktop window (session, file viewer,
 * todo / note lists… — see ./windows.ts) must not open connections of their
 * own: they would keep kicking each other off. Instead the main remote-desktop
 * window (`remote-desktop`, the OWNER) holds the only connection and serves the
 * pop-outs (PEERS) over local Tauri events:
 *
 *   peer ── emitTo(owner, MUX_REQ_EVENT, {from, msg}) ──► owner ── client.invoke / listen ──► B
 *   peer ◄── emitTo(peer, muxReplyEvent(peer), reply) ── owner ◄── ipc_result / ipc_event ── B
 *
 * - A peer runs its own IpcCore (queue, timeouts, refcounted subscriptions)
 *   over this local link: `welcome` = link up; `MUX_UP_EVENT` (the owner
 *   (re)started) or `MUX_DOWN_EVENT` (the owner is unloading) = link down, and
 *   the peer says `hello` again, which re-subscribes its events.
 * - The owner forwards invokes as-is and keeps one owner-side subscription per
 *   (peer, event); its own client refcounts them per event towards B, so B sees
 *   one controller with the union of every window's subscriptions.
 * - A peer's `hello` (or `bye`, or its window being destroyed) drops whatever
 *   the owner still held for it.
 *
 * Pure (no Vue / Tauri): the wiring lives in ./muxTransport.ts.
 */
import type { IpcFrame } from './ipcCore'

/** peer → owner (targeted at the owner window). Payload: {@link MuxEnvelope}. */
export const MUX_REQ_EVENT = 'remote-desktop:mux:req'
/** owner → everyone: the owner (re)started serving; peers say hello again. */
export const MUX_UP_EVENT = 'remote-desktop:mux:up'
/** owner → everyone: the owner is going away (reload / sign-out / close). */
export const MUX_DOWN_EVENT = 'remote-desktop:mux:down'

/** owner → one peer. Payload: {@link MuxReply}. */
export function muxReplyEvent(label: string): string {
  return `remote-desktop:mux:reply:${label}`
}

/** `n` tells a retried hello (same n: just answer) from a new one (drop and start over). */
export type MuxRequest = { t: 'hello'; n: number } | { t: 'bye' } | IpcFrame

export interface MuxEnvelope {
  from: string
  msg: MuxRequest
}

/** The owner's connection state, mirrored into the peers. */
export interface MuxState {
  status: string
  auth: string | null
  lastError: string | null
}

export type MuxReply =
  | { t: 'welcome'; n: number; state: MuxState }
  | { t: 'state'; state: MuxState }
  | { t: 'result'; id: string; ok: boolean; value?: unknown; error?: unknown; message?: string }
  | { t: 'event'; event: string; payload: unknown }

/** What the owner needs of its relay client. */
export interface MuxHostClient {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>
  listen<T>(event: string, handler: (e: { payload: T }) => void): Promise<() => void>
}

export interface MuxHostDeps {
  client: MuxHostClient
  /** Deliver a reply to one peer window. */
  send: (label: string, reply: MuxReply) => void
  initialState?: MuxState
}

/** Errors cross as data: an `Error` (local timeouts…) by message, anything else (B's own error value) as is. */
export function encodeError(e: unknown): { error?: unknown; message?: string } {
  return e instanceof Error ? { message: e.message } : { error: e }
}

export function decodeError(r: { error?: unknown; message?: string }): unknown {
  return typeof r.message === 'string' ? new Error(r.message) : r.error
}

/** The owner side: serves peer windows over its single client. */
export class MuxHost {
  private readonly deps: MuxHostDeps
  /** Per peer: event → owner-side unlisten. */
  private readonly peers = new Map<string, Map<string, Promise<() => void>>>()
  /** Per peer: the hello it was welcomed with. */
  private readonly hellos = new Map<string, number>()
  private state: MuxState

  constructor(deps: MuxHostDeps) {
    this.deps = deps
    this.state = deps.initialState ?? { status: 'idle', auth: null, lastError: null }
  }

  /** Peers that said hello and haven't left. */
  get peerLabels(): string[] {
    return [...this.peers.keys()]
  }

  /** Events subscribed on behalf of `label`. */
  eventsOf(label: string): string[] {
    return [...(this.peers.get(label)?.keys() ?? [])]
  }

  handle(from: string, msg: MuxRequest): void {
    if (!from || !msg) return
    if ('t' in msg) {
      if (msg.t === 'hello' && this.peers.has(from) && this.hellos.get(from) === msg.n) {
        // A retry of the hello we already answered: keep its subscriptions.
        this.deps.send(from, { t: 'welcome', n: msg.n, state: this.state })
        return
      }
      // A (re)booted peer starts from scratch; a leaving one is forgotten.
      this.drop(from)
      if (msg.t === 'hello') {
        this.peers.set(from, new Map())
        this.hellos.set(from, msg.n)
        this.deps.send(from, { t: 'welcome', n: msg.n, state: this.state })
      }
      return
    }
    switch (msg.action) {
      case 'ipc_invoke':
        this.deps.client.invoke(msg.cmd, msg.args as Record<string, unknown>).then(
          (value) => this.deps.send(from, { t: 'result', id: msg.id, ok: true, value }),
          (e: unknown) => this.deps.send(from, { t: 'result', id: msg.id, ok: false, ...encodeError(e) }),
        )
        return
      case 'ipc_listen': {
        let subs = this.peers.get(from)
        if (!subs) {
          subs = new Map()
          this.peers.set(from, subs)
        }
        if (subs.has(msg.event)) return
        const event = msg.event
        subs.set(
          event,
          this.deps.client
            .listen<unknown>(event, (e) => this.deps.send(from, { t: 'event', event, payload: e.payload }))
            .catch(() => () => {}),
        )
        return
      }
      case 'ipc_unlisten': {
        const subs = this.peers.get(from)
        const off = subs?.get(msg.event)
        if (!subs || !off) return
        subs.delete(msg.event)
        void off.then((f) => f())
        return
      }
    }
  }

  /** The owner's connection state changed: tell every peer. */
  setState(state: MuxState): void {
    this.state = state
    for (const label of this.peers.keys()) this.deps.send(label, { t: 'state', state })
  }

  /** Forget a peer and release its subscriptions. */
  drop(label: string): void {
    const subs = this.peers.get(label)
    this.hellos.delete(label)
    if (!subs) return
    this.peers.delete(label)
    for (const off of subs.values()) void off.then((f) => f())
  }

  close(): void {
    for (const label of [...this.peers.keys()]) this.drop(label)
  }
}

/** The part of IpcCore a peer drives. */
export interface MuxPeerCore {
  linkUp(): void
  linkDown(): void
  handleResult(id: string, ok: boolean, value: unknown, error: unknown): void
  handleEvent(event: string, payload: unknown): void
}

export interface MuxPeerDeps {
  core: MuxPeerCore
  sendHello: (n: number) => void
  onState?: (state: MuxState) => void
}

/** The pop-out side: an IpcCore whose "link" is the owner window. */
export class MuxPeer {
  private readonly deps: MuxPeerDeps
  private welcomed = false
  private n = 0

  constructor(deps: MuxPeerDeps) {
    this.deps = deps
  }

  /** Whether the owner has answered the last hello. */
  get connected(): boolean {
    return this.welcomed
  }

  /** (Re)announce this window: whatever was in flight on the old owner is lost. */
  hello(): void {
    this.down()
    this.n += 1
    this.deps.sendHello(this.n)
  }

  /** Resend the pending hello (no answer yet). */
  retryHello(): void {
    if (!this.welcomed) this.deps.sendHello(this.n)
  }

  /** The owner went away. */
  down(): void {
    this.welcomed = false
    this.deps.core.linkDown()
  }

  handle(reply: MuxReply): void {
    switch (reply.t) {
      case 'welcome':
        if (reply.n !== this.n) return
        this.deps.onState?.(reply.state)
        if (this.welcomed) return
        this.welcomed = true
        this.deps.core.linkUp()
        return
      case 'state':
        this.deps.onState?.(reply.state)
        return
      case 'result':
        this.deps.core.handleResult(reply.id, reply.ok, reply.value, reply.ok ? undefined : decodeError(reply))
        return
      case 'event':
        this.deps.core.handleEvent(reply.event, reply.payload)
        return
    }
  }
}
