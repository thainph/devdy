// Label / URL of a pop-out window as opened from THIS window. From a
// remote-desktop window (`?remote=1`) a pop-out belongs to the remote set: its
// label is prefixed `remote-` so it never collides with (or finds) this
// machine's own window of the same kind, and its URL carries `remote=1` so it
// boots against the controlled Devdy too (see lib/ipc/windows.ts, lib/ipc/mux.ts).
// Every lib/*Window.ts helper routes its labels and URLs through here.
import { isRemoteMode } from '@/lib/ipc/remote'
import { toRemoteLabel, withRemoteParam } from '@/lib/ipc/windows'

export function popoutLabel(label: string): string {
  return isRemoteMode() ? toRemoteLabel(label) : label
}

export function popoutUrl(url: string): string {
  return isRemoteMode() ? withRemoteParam(url) : url
}
