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
  Settings,
  Info,
} from 'lucide-vue-next'

/**
 * The app's top-level destinations, in sidebar order. Shared by the sidebar and
 * the native "Go" menu so the two can never drift — adding a screen in one
 * place adds it to both.
 *
 * Ordered by how often they're used: daily work first (so it gets ⌘1…⌘4),
 * then the agent configuration library, then app-level screens. A divider is
 * drawn wherever `group` changes.
 */
export type NavGroup = 'work' | 'config' | 'app'

export interface NavRoute {
  path: string
  /** i18n key under the `nav` namespace. */
  labelKey: string
  icon: Component
  group: NavGroup
}

export const NAV_ROUTES: NavRoute[] = [
  { path: '/projects', labelKey: 'nav.projects', icon: FolderOpen, group: 'work' },
  { path: '/pr-inbox', labelKey: 'nav.prInbox', icon: GitPullRequest, group: 'work' },
  { path: '/calendar', labelKey: 'nav.calendar', icon: CalendarDays, group: 'work' },
  { path: '/gantt', labelKey: 'nav.gantt', icon: GanttChartSquare, group: 'work' },
  { path: '/skills', labelKey: 'nav.skills', icon: Puzzle, group: 'config' },
  { path: '/rules', labelKey: 'nav.rules', icon: ScrollText, group: 'config' },
  { path: '/mcp', labelKey: 'nav.mcp', icon: Server, group: 'config' },
  { path: '/servers', labelKey: 'nav.servers', icon: HardDrive, group: 'config' },
  { path: '/stats', labelKey: 'nav.stats', icon: BarChart3, group: 'app' },
  { path: '/settings', labelKey: 'nav.settings', icon: Settings, group: 'app' },
  { path: '/about', labelKey: 'nav.about', icon: Info, group: 'app' },
]

/** True when `NAV_ROUTES[index]` starts a new group (and so gets a divider). */
export function startsNavGroup(index: number): boolean {
  return index > 0 && NAV_ROUTES[index].group !== NAV_ROUTES[index - 1].group
}
