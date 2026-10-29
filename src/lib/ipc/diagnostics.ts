/**
 * Remote-desktop diagnostics: a remote window on the controlling machine has no
 * devtools in release builds, so its console errors and a few IPC traces are
 * sent to the CONTROLLED machine's log (`remote_client_log`), where they can be
 * read after the fact. Best-effort and throttled; never throws.
 */
import { getCurrentWindow } from '@tauri-apps/api/window'
import { remoteIpc } from './remote'

type Level = 'info' | 'warn' | 'error'

const MAX_PER_MINUTE = 120
let windowStart = 0
let sentInWindow = 0

function label(): string {
  try {
    return getCurrentWindow().label
  } catch {
    return 'unknown'
  }
}

function describe(value: unknown): string {
  if (value instanceof Error) return `${value.name}: ${value.message}${value.stack ? `\n${value.stack}` : ''}`
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

/** Send one line to the controlled machine's log. */
export function remoteLog(level: Level, message: string): void {
  const client = remoteIpc()
  if (!client) return
  const now = Date.now()
  if (now - windowStart > 60_000) {
    windowStart = now
    sentInWindow = 0
  }
  if (++sentInWindow > MAX_PER_MINUTE) return
  void client.invoke('remote_client_log', { level, source: label(), message }).catch(() => {})
}

/** Trace a window-to-window event of interest (kept to low-volume names). */
export function traceBusEvent(kind: 'emit' | 'listen' | 'recv', event: string, detail = ''): void {
  if (!event.startsWith('permission:')) return
  remoteLog('info', `bus ${kind} ${event}${detail ? ` ${detail}` : ''}`)
}

let installed = false

/** Forward this window's console errors/warnings and uncaught failures. */
export function installRemoteDiagnostics(): void {
  if (installed || typeof window === 'undefined') return
  installed = true
  window.addEventListener('error', (e) => remoteLog('error', `uncaught: ${describe(e.error ?? e.message)}`))
  window.addEventListener('unhandledrejection', (e) => remoteLog('error', `unhandled rejection: ${describe(e.reason)}`))
  for (const level of ['error', 'warn'] as const) {
    const original = console[level].bind(console)
    console[level] = (...args: unknown[]) => {
      original(...args)
      remoteLog(level, args.map(describe).join(' '))
    }
  }
  remoteLog('info', `remote window booted: ${location.search}`)
}
