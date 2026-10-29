/**
 * `@tauri-apps/api/core` as the app sees it (Vite alias). Identical to the real
 * module, except `invoke` is routed to the controlled Devdy when this window is
 * a remote-desktop window (see ./remote.ts). Plugin commands (`plugin:*`:
 * windows, webviews, dialogs, opener…) stay local, except the few that act on
 * the controlled machine's paths, and `convertFileSrc` serves that machine's
 * files (see ./native.ts).
 */
import {
  convertFileSrc as realConvertFileSrc,
  invoke as realInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from '@tauri-real/core'
import { interceptNative, remoteFileSrc } from './native'
import { remoteIpc } from './remote'

export * from '@tauri-real/core'

/**
 * App commands that act on the machine the user is SITTING at, not on the
 * controlled one: notifications and the mascot's voice belong on A's screen.
 */
const SEAT_LOCAL_COMMANDS = new Set([
  'show_permission_notification',
  'show_run_finished_notification',
  'mascot_speak',
  'cancel_mascot_speak',
])
/**
 * App commands a remote window must not run at all (they resolve `undefined`):
 * - `set_app_menu`: the native menu bar is owned by each machine's own main
 *   window, so a remote window rebuilding it would clobber either the
 *   controlled machine's menu or the local one.
 * - `start_file_drag`: acts on the CALLING window, which on the controlled
 *   machine is its hidden bridge — the native drag would start there and the
 *   call would hang until it ends. Run locally it has no B paths to drag, so
 *   dragging a file out of a remote window is simply not offered.
 * Other commands that take the calling window/webview (`rdfile_register`) are
 * local plumbing of the shim, called with the real invoke.
 */
const REMOTE_SKIPPED_COMMANDS = new Set(['set_app_menu', 'start_file_drag'])

export async function invoke<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  const remote = remoteIpc()
  if (remote) {
    if (REMOTE_SKIPPED_COMMANDS.has(cmd)) return undefined as T
    if (SEAT_LOCAL_COMMANDS.has(cmd)) return realInvoke<T>(cmd, args, options)
    if (!cmd.startsWith('plugin:')) return remote.invoke<T>(cmd, args)
    const native = interceptNative(cmd, args)
    if (native) return native as Promise<T>
  }
  return realInvoke<T>(cmd, args, options)
}

export function convertFileSrc(filePath: string, protocol = 'asset'): string {
  if (remoteIpc() && protocol === 'asset') return remoteFileSrc(filePath)
  return realConvertFileSrc(filePath, protocol)
}
