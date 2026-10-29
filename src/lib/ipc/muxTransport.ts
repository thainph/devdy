/**
 * Tauri wiring of ./mux.ts: the main remote-desktop window serves its relay
 * connection to the remote pop-outs over local events (real event API, never
 * the shim, so nothing here is routed to the controlled Devdy).
 */
import { emit, emitTo, listen, type UnlistenFn } from '@tauri-real/event'
import type { InvokeArgs } from '@tauri-real/core'
import type { EventCallback } from '@tauri-real/event'
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ref, watch } from 'vue'
import type { AuthState, ConnStatus } from '@/controller/connection'
import { IpcCore } from './ipcCore'
import {
  MUX_DOWN_EVENT,
  MUX_REQ_EVENT,
  MUX_UP_EVENT,
  MuxHost,
  MuxPeer,
  muxReplyEvent,
  type MuxEnvelope,
  type MuxReply,
  type MuxRequest,
  type MuxState,
} from './mux'
import type { RemoteIpcClient, RemoteIpcState } from './remote'
import { REMOTE_OWNER_LABEL } from './windows'

/** A peer resends its hello this often until the owner answers. */
const HELLO_RETRY_MS = 1500

function snapshot(state: RemoteIpcState): MuxState {
  return { status: state.status.value, auth: state.auth.value, lastError: state.lastError.value }
}

/**
 * Owner side, in the `remote-desktop` window: serve `client` to the remote
 * pop-outs. Returns the teardown.
 */
export async function startMuxHost(client: RemoteIpcClient): Promise<() => void> {
  const host = new MuxHost({
    client,
    send: (label, reply) => void emitTo(label, muxReplyEvent(label), reply).catch(() => {}),
    initialState: snapshot(client.state),
  })
  // A pop-out that is closed without a `bye` (closing a window does not
  // reliably unload its page) is dropped when its window goes away.
  const watched = new Set<string>()
  const watchPeer = (label: string): void => {
    if (watched.has(label)) return
    watched.add(label)
    void WebviewWindow.getByLabel(label).then((win) => {
      if (!win) {
        watched.delete(label)
        host.drop(label)
        return
      }
      void win.once('tauri://destroyed', () => {
        watched.delete(label)
        host.drop(label)
      })
    })
  }

  const offReq = await listen<MuxEnvelope>(MUX_REQ_EVENT, (e) => {
    const { from, msg } = e.payload ?? ({} as MuxEnvelope)
    if (typeof from !== 'string' || !msg) return
    host.handle(from, msg)
    if ('t' in msg && msg.t === 'hello') watchPeer(from)
  })
  const stopState = watch(
    () => snapshot(client.state),
    (s) => host.setState(s),
    { flush: 'sync' },
  )
  const onUnload = (): void => void emit(MUX_DOWN_EVENT).catch(() => {})
  window.addEventListener('pagehide', onUnload)
  // Pop-outs that outlived a reload of this window say hello again.
  await emit(MUX_UP_EVENT).catch(() => {})

  return () => {
    window.removeEventListener('pagehide', onUnload)
    stopState()
    offReq()
    host.close()
    onUnload()
  }
}

function newId(): string {
  return typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `mux-${Date.now()}-${Math.random().toString(36).slice(2)}`
}

/**
 * Peer side, in a remote pop-out labelled `label`: a client that reaches the
 * controlled Devdy through the owner window. The pop-out closes itself when the
 * owner window goes away (or is not there at all).
 */
export function startMuxClient(label: string): RemoteIpcClient {
  const state: RemoteIpcState = {
    status: ref<ConnStatus>('idle'),
    auth: ref<AuthState | null>(null),
    lastError: ref<string | null>(null),
  }
  const send = (msg: MuxRequest): boolean => {
    void emitTo(REMOTE_OWNER_LABEL, MUX_REQ_EVENT, { from: label, msg } satisfies MuxEnvelope).catch(() => {})
    return true
  }
  const core = new IpcCore({ send, newId })
  const peer = new MuxPeer({
    core,
    sendHello: (n) => send({ t: 'hello', n }),
    onState: (s) => {
      state.status.value = s.status as ConnStatus
      state.auth.value = s.auth as AuthState | null
      state.lastError.value = s.lastError
    },
  })

  let closed = false
  const unlisteners: UnlistenFn[] = []
  const retry = setInterval(() => peer.retryHello(), HELLO_RETRY_MS)
  const closeSelf = (): void => void getCurrentWindow().close().catch(() => {})

  void (async () => {
    // Listen before saying hello, or the welcome could slip past.
    unlisteners.push(await listen<MuxReply>(muxReplyEvent(label), (e) => peer.handle(e.payload)))
    unlisteners.push(await listen(MUX_UP_EVENT, () => peer.hello()))
    unlisteners.push(await listen(MUX_DOWN_EVENT, () => peer.down()))
    if (closed) return
    peer.hello()
    const owner = await WebviewWindow.getByLabel(REMOTE_OWNER_LABEL)
    if (!owner) closeSelf()
    else unlisteners.push(await owner.once('tauri://destroyed', closeSelf))
  })().catch((e) => console.error('[remote-ipc] pop-out link failed', e))

  window.addEventListener('pagehide', () => send({ t: 'bye' }))

  return {
    state,
    invoke: <T>(cmd: string, args?: InvokeArgs) => core.invoke<T>(cmd, args),
    listen: <T>(event: string, handler: EventCallback<T>) =>
      Promise.resolve<UnlistenFn>(core.listen<T>(event, handler)),
    close: () => {
      if (closed) return
      closed = true
      clearInterval(retry)
      send({ t: 'bye' })
      unlisteners.forEach((off) => off())
      core.close()
    },
  }
}
