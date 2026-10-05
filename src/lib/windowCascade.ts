// Where to place a newly opened pop-out (note / todo editor, file viewer, gantt,
// session, permission) so it lands on the SAME monitor as the window the user is
// acting from — and, for the cascading kinds, offset from the pop-outs already
// open instead of dead-on top of them.
//
// Without a position, a pop-out opens at the OS-default spot, which on a
// multi-monitor setup is the primary screen — not the monitor the main window (or
// a session pop-out) is on. Both helpers below anchor to the current window's
// monitor so a window always appears where work is happening.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { currentMonitor } from '@tauri-apps/api/window'

const POP_OUT_PREFIXES = ['item-create', 'item-edit-', 'fileviewer-']
const STEP = 32 // logical px between successive windows
const WRAP = 8 // restart the cascade after this many so it stays on-screen
const BASE_X = 160
const BASE_Y = 120

/** Logical bounds of the monitor the current window is on, or null if unknown. */
async function currentMonitorBounds(): Promise<{
  x: number
  y: number
  width: number
  height: number
} | null> {
  try {
    // `currentMonitor()` resolves against the calling webview's window, so this is
    // the monitor the user is acting from (main, or a popped-out session).
    const monitor = await currentMonitor()
    if (!monitor) return null
    // Convert the monitor's own physical bounds with its own scale factor, so a
    // second monitor at a different DPI still maps to the right logical origin.
    const factor = monitor.scaleFactor
    const pos = monitor.position.toLogical(factor)
    const size = monitor.size.toLogical(factor)
    return { x: pos.x, y: pos.y, width: size.width, height: size.height }
  } catch {
    return null
  }
}

/**
 * Logical (x, y) for the next cascading pop-out: offset from the top-left of the
 * CURRENT monitor, then stepped past the ones already open (wrapping so the
 * cascade never marches off-screen).
 */
export async function nextCascadePosition(): Promise<{ x: number; y: number }> {
  let count = 0
  try {
    const all = await WebviewWindow.getAll()
    count = all.filter((w) => POP_OUT_PREFIXES.some((p) => w.label.startsWith(p))).length
  } catch {
    /* fall back to the base corner */
  }
  const i = count % WRAP
  const monitor = await currentMonitorBounds()
  const originX = (monitor?.x ?? 0) + BASE_X
  const originY = (monitor?.y ?? 0) + BASE_Y
  return { x: originX + i * STEP, y: originY + i * STEP }
}

/**
 * Logical (x, y) that centres a window of `width` × `height` on the CURRENT
 * monitor. Spread into the `WebviewWindow` options; returns `undefined` when the
 * monitor can't be read, so the OS default placement is kept.
 */
export async function centerOnCurrentMonitor(
  width: number,
  height: number,
): Promise<{ x: number; y: number } | undefined> {
  const monitor = await currentMonitorBounds()
  if (!monitor) return undefined
  return {
    x: Math.round(monitor.x + (monitor.width - width) / 2),
    y: Math.round(monitor.y + (monitor.height - height) / 2),
  }
}

/**
 * Place a `width`-wide window flush against the right edge of the CURRENT monitor,
 * tall enough to span (almost) its full height — so it reads like a side panel
 * beside the main window without resizing it. Returns the logical x/y plus the
 * computed height; `undefined` when the monitor can't be read.
 */
export async function dockRightOnCurrentMonitor(
  width: number,
  margin = 24,
): Promise<{ x: number; y: number; height: number } | undefined> {
  const monitor = await currentMonitorBounds()
  if (!monitor) return undefined
  return {
    x: Math.round(monitor.x + monitor.width - width - margin),
    y: Math.round(monitor.y + margin),
    height: Math.round(monitor.height - margin * 2),
  }
}
