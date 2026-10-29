/**
 * Remote desktop IPC client (CONTRACT — implemented by the shim task).
 *
 * A remote-desktop window (opened with `?remote=1`) runs the normal UI, but its
 * app-level `invoke` / `listen` go to another Devdy over the relay instead of
 * this machine's backend. `remoteIpc()` is `null` everywhere else, so the shim
 * in ./core.ts and ./event.ts is a pure pass-through.
 */
import type { EventCallback, UnlistenFn } from '@tauri-real/event'
import type { InvokeArgs } from '@tauri-real/core'
import type { Ref } from 'vue'
import type { AuthState, ConnStatus } from '@/controller/connection'
import type { SessionLinkPayload } from '@/controller/protocol'

/** Live connection state for the remote-desktop banner. */
export interface RemoteIpcState {
  status: Ref<ConnStatus>
  auth: Ref<AuthState | null>
  lastError: Ref<string | null>
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

/**
 * Connect this remote-desktop window to the controlled Devdy. `onToken` is
 * called with the (re)issued session token so the caller can persist it.
 */
export function startRemoteIpc(
  _link: SessionLinkPayload,
  _token: string,
  _onToken?: (token: string) => void,
): RemoteIpcClient {
  void _link
  void _token
  void _onToken
  throw new Error('remote IPC client not implemented yet')
}

/** Test seam: install a client directly. */
export function setRemoteIpcForTest(c: RemoteIpcClient | null): void {
  client = c
}
