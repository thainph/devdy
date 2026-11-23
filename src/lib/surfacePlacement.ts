// Surface OS windows: whether one is open, and opening one (a tab popped out,
// or ⌘-click on a file link). No router import here: the router's guards use this module, and
// lib/surfaceOpen.ts (which does import the router) builds on it.
import { availableMonitors } from '@tauri-apps/api/window'
import { WebviewWindow, getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import type { CalEvent } from '@/stores/googleCalendar'
import type { SurfaceKind, SurfaceParams } from '@/lib/surfaces'
import { PR_INBOX_WINDOW_LABEL, openPrInboxWindow } from '@/lib/prInboxWindow'
import { GITHUB_MENTIONS_WINDOW_LABEL, openGithubMentionsWindow } from '@/lib/githubMentionsWindow'
import { CALENDAR_WINDOW_LABEL, openCalendarWindow } from '@/lib/calendarWindow'
import { DELIVERY_RADAR_WINDOW_LABEL, openDeliveryRadarWindow } from '@/lib/deliveryRadarWindow'
import { SLACK_WINDOW_LABEL, openSlackWindow } from '@/lib/slackWindow'
import { itemListWindowLabel, openItemListWindow } from '@/lib/itemListWindow'
import { fileWindowLabel, openFileWindow } from '@/lib/fileWindow'

export function isMainWindow(): boolean {
  try {
    return getCurrentWebviewWindow().label === 'main'
  } catch {
    return true
  }
}

/** OS window label of a surface (one per surface; files one per file). */
function surfaceWindowLabel(kind: SurfaceKind, params: SurfaceParams): string | null {
  switch (kind) {
    case 'prInbox': return PR_INBOX_WINDOW_LABEL
    case 'mentions': return GITHUB_MENTIONS_WINDOW_LABEL
    case 'calendar': return CALENDAR_WINDOW_LABEL
    case 'radar': return DELIVERY_RADAR_WINDOW_LABEL
    case 'slack': return SLACK_WINDOW_LABEL
    case 'todos': return itemListWindowLabel('todo')
    case 'notes': return itemListWindowLabel('note')
    case 'file':
      return params.projectPath && params.path ? fileWindowLabel(params.projectPath, params.path) : null
  }
}

/** True when the surface already has its own window open (it is re-used, not doubled by a tab). */
export async function surfaceWindowOpen(kind: SurfaceKind, params: SurfaceParams = {}): Promise<boolean> {
  const label = surfaceWindowLabel(kind, params)
  if (!label) return false
  try {
    return (await WebviewWindow.getByLabel(label)) !== null
  } catch {
    return false
  }
}

// Monitor count changes rarely; don't ask the OS on every click.
let monitorCache: { count: number; at: number } | null = null
const MONITOR_TTL_MS = 3000

export async function monitorCount(fresh = false): Promise<number> {
  if (!fresh && monitorCache && Date.now() - monitorCache.at < MONITOR_TTL_MS) return monitorCache.count
  let count = 1
  try {
    count = Math.max(1, (await availableMonitors()).length)
  } catch {
    /* outside the Tauri shell: behave like one screen */
  }
  monitorCache = { count, at: Date.now() }
  return count
}

/** Extras a surface can be opened with that don't belong in a tab's params. */
export interface SurfaceExtras {
  /** Calendar: event whose detail drawer to open. */
  calendarEvent?: CalEvent
  /** Slack: open the add-thread dialog. */
  slackAdd?: boolean
}

/** Open (or focus) the surface in its own OS window. */
export async function openSurfaceWindow(
  kind: SurfaceKind,
  params: SurfaceParams = {},
  extras: SurfaceExtras = {},
): Promise<void> {
  switch (kind) {
    case 'prInbox':
      await openPrInboxWindow()
      return
    case 'mentions':
      await openGithubMentionsWindow()
      return
    case 'calendar':
      await openCalendarWindow(extras.calendarEvent ? { event: extras.calendarEvent } : {})
      return
    case 'radar':
      await openDeliveryRadarWindow(params.project)
      return
    case 'slack':
      await openSlackWindow({ id: params.id, projectId: params.projectId ?? null, add: extras.slackAdd })
      return
    case 'todos':
    case 'notes':
      await openItemListWindow(kind === 'todos' ? 'todo' : 'note', {
        id: params.id ?? null,
        projectId: params.projectId ?? null,
      })
      return
    case 'file':
      if (params.projectPath && params.path) {
        await openFileWindow(params.projectPath, params.path, params.line ? Number(params.line) : null)
      }
      return
  }
}
