/**
 * Remote desktop IPC client (CONTRACT — implemented by the shim task).
 *
 * A remote-desktop window (opened with `?remote=1`) runs the normal UI, but its
 * app-level `invoke` / `listen` go to another Devdy over the relay instead of
 * this machine's backend. `remoteIpc()` is `null` everywhere else, so the shim
 * in ./core.ts and ./event.ts is a pure pass-through.
 *
 * Transport is a `ControllerConnection` in IPC mode (same relay, E2E and
 * reconnect as the web Controller), imported lazily: every window loads this
 * module through the shim, and only a remote-desktop window needs the
 * transport and its crypto. The call/subscription bookkeeping lives in
 * ./ipcCore.ts.
 */
import type { EventCallback, UnlistenFn } from '@tauri-real/event'
import type { InvokeArgs } from '@tauri-real/core'
import { ref, watch, type Ref } from 'vue'
import type { AuthState, ConnStatus, ControllerConnection } from '@/controller/connection'
import type { SessionLinkPayload, StreamPayload } from '@/controller/protocol'
import { IpcCore } from './ipcCore'

/** Live connection state for the remote-desktop banner. */
export interface RemoteIpcState {
  status: Ref<ConnStatus>
  auth: Ref<AuthState | null>
  lastError: Ref<string | null>
  /** Another device (or tab) took B's session over and this window stopped
   * instead of fighting it; only a deliberate reconnect takes it back. */
  superseded: Ref<boolean>
}

export interface RemoteIpcClient {
  readonly state: RemoteIpcState
  /** Run a Tauri command on the controlled Devdy (queued until connected). */
  invoke<T>(cmd: string, args?: InvokeArgs): Promise<T>
  /** Subscribe to an app event on the controlled Devdy. */
  listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn>
  /** Disconnect for good. */
  close(): void
}

/** Whether this window is a remote-desktop window. */
export function isRemoteMode(): boolean {
  return typeof location !== 'undefined' && new URLSearchParams(location.search).get('remote') === '1'
}

let client: RemoteIpcClient | null = null

/** The active client in a remote-desktop window, else `null`. */
export function remoteIpc(): RemoteIpcClient | null {
  return client
}

function newId(): string {
  return typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `ipc-${Date.now()}-${Math.random().toString(36).slice(2)}`
}

/**
 * Connect this remote-desktop window to the controlled Devdy. `onToken` is
 * called with the (re)issued session token so the caller can persist it.
 *
 * Calls made before the first authentication are queued, not failed, so the
 * app can boot against the client right away. Rejections: the command's own
 * error value (as Tauri's `invoke`), or `Error('remote_timeout')` after 60 s,
 * `Error('remote_connection_lost')` if the link dropped mid-call,
 * `Error('remote_closed')` after {@link RemoteIpcClient.close}.
 */
export function startRemoteIpc(
  link: SessionLinkPayload,
  token: string,
  onToken?: (token: string) => void,
): RemoteIpcClient {
  client?.close()

  let conn: ControllerConnection | null = null
  let closed = false
  const state: RemoteIpcState = {
    status: ref<ConnStatus>('idle'),
    auth: ref<AuthState | null>(null),
    lastError: ref<string | null>(null),
    superseded: ref(false),
  }
  const core = new IpcCore({
    send: (frame) => conn?.sendCommand(frame) ?? false,
    newId,
  })
  const onPayload = (p: StreamPayload): void => {
    if (p.kind === 'ipc_result') core.handleResult(p.id, p.ok, p.value, p.error)
    else if (p.kind === 'ipc_event') core.handleEvent(p.event, p.payload)
    // Anything else is run-streaming traffic an IPC Controller does not ask for.
  }

  const c: RemoteIpcClient = {
    state,
    invoke: <T>(cmd: string, args?: InvokeArgs) => core.invoke<T>(cmd, args),
    listen: <T>(event: string, handler: EventCallback<T>) =>
      Promise.resolve<UnlistenFn>(core.listen<T>(event, handler)),
    close: () => {
      closed = true
      conn?.close()
      core.close()
      if (client === c) client = null
    },
  }
  client = c

  const fail = (e: unknown): void => {
    state.lastError.value = e instanceof Error ? e.message : String(e)
    state.status.value = 'error'
  }
  void import('@/controller/connection')
    .then(async ({ ControllerConnection }) => {
      if (closed) return
      conn = new ControllerConnection(link, { mode: 'session', token }, onPayload, {
        ipc: true,
        onToken: (t) => {
          core.linkUp()
          onToken?.(t)
        },
        onLinkDown: () => core.linkDown(),
      })
      const src = conn.state
      watch(src.status, (v) => (state.status.value = v), { flush: 'sync', immediate: true })
      watch(src.auth, (v) => (state.auth.value = v), { flush: 'sync', immediate: true })
      watch(src.lastError, (v) => (state.lastError.value = v), { flush: 'sync', immediate: true })
      watch(src.superseded, (v) => (state.superseded.value = v), { flush: 'sync', immediate: true })
      // Only the E2E module init can fail here; the transport reports the rest.
      await conn.connect()
    })
    .catch(fail)
  return c
}

/**
 * Install a client that is not a direct relay connection: a remote pop-out's
 * client reaches the controlled Devdy through the main remote-desktop window
 * (./muxTransport.ts).
 */
export function installRemoteIpc(c: RemoteIpcClient): void {
  client?.close()
  client = c
}

/** Test seam: install a client directly. */
export function setRemoteIpcForTest(c: RemoteIpcClient | null): void {
  client = c
}
