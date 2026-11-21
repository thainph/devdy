// Where a surface opens right now (tab vs window), and opening one in its own
// OS window. No router import here: the router's guards use this module, and
// lib/surfaceOpen.ts (which does import the router) builds on it.
import { availableMonitors } from '@tauri-apps/api/window'
import { useAppSettingsStore } from '@/stores/appSettings'
import type { CalEvent } from '@/stores/googleCalendar'
import {
  parseOverrides,
  parseWindowMode,
  placementFor,
  type Placement,
  type SurfaceKind,
  type SurfaceParams,
} from '@/lib/surfaces'
import { openPrInboxWindow } from '@/lib/prInboxWindow'
import { openGithubMentionsWindow } from '@/lib/githubMentionsWindow'
import { openCalendarWindow } from '@/lib/calendarWindow'
import { openDeliveryRadarWindow } from '@/lib/deliveryRadarWindow'
import { openSlackWindow } from '@/lib/slackWindow'
import { openItemListWindow } from '@/lib/itemListWindow'
import { openFileWindow } from '@/lib/fileWindow'

// The surface routes' guards divert to the window when the placement says so;
// a tab opened on purpose (docking, "open as tab", switching tabs) must get
// through: lib/surfaceOpen adds the kind around its router.push.
export const forcedTab = new Set<SurfaceKind>()

/** True (once) when the next navigation to `kind` was explicitly asked as a tab. */
export function takeForcedTab(kind: SurfaceKind): boolean {
  return forcedTab.delete(kind)
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

/** The tab/window decision for `kind` with the current settings and monitors. */
export async function resolvePlacement(kind: SurfaceKind): Promise<Placement> {
  const settings = useAppSettingsStore()
  try {
    await settings.ensureLoaded()
  } catch {
    /* settings unavailable: defaults below */
  }
  const s = settings.settings
  return placementFor(
    kind,
    parseWindowMode(s?.window_mode),
    parseOverrides(s?.window_mode_overrides),
    await monitorCount(),
  )
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
