import type { Component } from 'vue'
import {
  Puzzle,
  ScrollText,
  Server,
  HardDrive,
  FolderOpen,
  GitPullRequest,
  GanttChartSquare,
  BarChart3,
  CalendarDays,
  Activity,
  Settings,
  Info,
} from 'lucide-vue-next'
import { openPrInboxWindow } from '@/lib/prInboxWindow'

/**
 * The app's top-level destinations, in sidebar order. Shared by the sidebar and
 * the native "Go" menu so the two can never drift — adding a screen in one
 * place adds it to both.
 *
 * Ordered by how often they're used: daily work first, then the agent
 * configuration library, then app-level screens. ⌘1…⌘9 are fixed per route
 * (`shortcut`), not by position. A divider is
 * drawn wherever `group` changes.
 */
export type NavGroup = 'work' | 'config' | 'app'

export interface NavRoute {
  path: string
  /** i18n key under the `nav` namespace. */
  labelKey: string
  icon: Component
  group: NavGroup
  /**
   * Set for destinations that live in their own OS window (like the Slack
   * threads window): the sidebar entry, the Go menu item and its shortcut call
   * this instead of routing, and the route itself diverts to it.
   */
  openWindow?: () => unknown
  /**
   * ⌘/Ctrl + this digit in the Go menu. Fixed per destination so adding a
   * screen never moves the shortcuts people already know.
   */
  shortcut?: number
}

export const NAV_ROUTES: NavRoute[] = [
  { path: '/projects', labelKey: 'nav.projects', icon: FolderOpen, group: 'work', shortcut: 1 },
  { path: '/pr-inbox', labelKey: 'nav.prInbox', icon: GitPullRequest, group: 'work', openWindow: openPrInboxWindow, shortcut: 2 },
  { path: '/calendar', labelKey: 'nav.calendar', icon: CalendarDays, group: 'work', shortcut: 3 },
  { path: '/gantt', labelKey: 'nav.gantt', icon: GanttChartSquare, group: 'work', shortcut: 4 },
  { path: '/pulse', labelKey: 'nav.pulse', icon: Activity, group: 'work' },
  { path: '/skills', labelKey: 'nav.skills', icon: Puzzle, group: 'config', shortcut: 5 },
  { path: '/rules', labelKey: 'nav.rules', icon: ScrollText, group: 'config', shortcut: 6 },
  { path: '/mcp', labelKey: 'nav.mcp', icon: Server, group: 'config', shortcut: 7 },
  { path: '/servers', labelKey: 'nav.servers', icon: HardDrive, group: 'config', shortcut: 8 },
  { path: '/stats', labelKey: 'nav.stats', icon: BarChart3, group: 'app', shortcut: 9 },
  { path: '/settings', labelKey: 'nav.settings', icon: Settings, group: 'app' },
  { path: '/about', labelKey: 'nav.about', icon: Info, group: 'app' },
]

/** True when `NAV_ROUTES[index]` starts a new group (and so gets a divider). */
export function startsNavGroup(index: number): boolean {
  return index > 0 && NAV_ROUTES[index].group !== NAV_ROUTES[index - 1].group
}
