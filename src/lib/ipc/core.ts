/**
 * `@tauri-apps/api/core` as the app sees it (Vite alias). Identical to the real
 * module, except `invoke` is routed to the controlled Devdy when this window is
 * a remote-desktop window (see ./remote.ts). Plugin commands (`plugin:*`:
 * windows, webviews, dialogs, opener…) always stay local.
 */
import { invoke as realInvoke, type InvokeArgs, type InvokeOptions } from '@tauri-real/core'
import { remoteIpc } from './remote'

export * from '@tauri-real/core'

export async function invoke<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  const remote = remoteIpc()
  if (remote && !cmd.startsWith('plugin:')) return remote.invoke<T>(cmd, args)
  return realInvoke<T>(cmd, args, options)
}
