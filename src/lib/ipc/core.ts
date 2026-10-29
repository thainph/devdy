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

export async function invoke<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  const remote = remoteIpc()
  if (remote) {
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
