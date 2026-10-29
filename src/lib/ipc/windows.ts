/**
 * Naming rules of the remote-desktop windows on the controlling machine (A),
 * free of Vue / Tauri so they can be unit-tested in plain Node.
 *
 * - The main remote-desktop window is `remote-desktop` (lib/remoteDesktopWindow.ts).
 *   It owns the one relay connection; see ./mux.ts.
 * - Every pop-out opened from a remote window gets the same label it would have
 *   locally, prefixed with `remote-` (`session-1ab` → `remote-session-1ab`), and
 *   `remote=1` in its URL. A's own local windows keep their labels, so the two
 *   sets never collide and lookups by label stay inside one set. "The main
 *   window" of the remote set is `remote-desktop`.
 * - Frontend-to-frontend events (`emit` / `listen` between windows) of the
 *   remote set travel locally under a `remote-desktop:bus:` prefix, which A's
 *   local windows never listen to (./event.ts).
 */

export const REMOTE_OWNER_LABEL = 'remote-desktop'
export const REMOTE_LABEL_PREFIX = 'remote-'
const BUS_PREFIX = 'remote-desktop:bus:'

/** Whether `label` belongs to the remote-desktop window set. */
export function isRemoteLabel(label: string): boolean {
  return label.startsWith(REMOTE_LABEL_PREFIX)
}

/** The remote-set counterpart of a local window label (idempotent). */
export function toRemoteLabel(label: string): string {
  if (label === 'main') return REMOTE_OWNER_LABEL
  return isRemoteLabel(label) ? label : `${REMOTE_LABEL_PREFIX}${label}`
}

/** The local label a remote-set label stands for (`remote-desktop` → `main`). */
export function fromRemoteLabel(label: string): string {
  if (label === REMOTE_OWNER_LABEL) return 'main'
  return isRemoteLabel(label) ? label.slice(REMOTE_LABEL_PREFIX.length) : label
}

/** `url` (an app page, e.g. `index.html?x=1`) with `remote=1` set. */
export function withRemoteParam(url: string): string {
  const hash = url.indexOf('#')
  const base = hash >= 0 ? url.slice(0, hash) : url
  const frag = hash >= 0 ? url.slice(hash) : ''
  const q = base.indexOf('?')
  const path = q >= 0 ? base.slice(0, q) : base
  const params = new URLSearchParams(q >= 0 ? base.slice(q + 1) : '')
  params.set('remote', '1')
  return `${path}?${params.toString()}${frag}`
}

/** The local event name carrying app event `event` between remote windows. */
export function busEventName(event: string): string {
  return `${BUS_PREFIX}${event}`
}
