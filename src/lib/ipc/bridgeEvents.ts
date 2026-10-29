/**
 * Events the remote-desktop bridge never forwards to the Controller, whatever
 * it asks for. The bridge already listens targeted at its own label, so events
 * `emit_to`'d at this machine's windows do not reach it; this list is the
 * backstop for the ones that only make sense on this machine: app-menu and
 * tray clicks (they would make the Controller's UI act on a local click), and
 * the bridge's own request channel.
 */
const DENIED_EVENTS = new Set([
  // src-tauri/src/commands/app_menu.rs MENU_ACTION_EVENT
  'menu://action',
  // src-tauri/src/commands/tray.rs OPEN_PROCS_EVENT / OPEN_RUN_EVENT
  'tray:open-processes',
  'tray://open-run',
])

const DENIED_PREFIXES = ['remote-bridge://', 'tauri://']

export function isBridgeDeniedEvent(event: string): boolean {
  return DENIED_EVENTS.has(event) || DENIED_PREFIXES.some((p) => event.startsWith(p))
}
