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
  CalendarClock,
  CalendarDays,
  Settings,
  Info,
} from 'lucide-vue-next'

/**
 * The app's top-level destinations, in sidebar order. Shared by the sidebar and
 * the native "Go" menu so the two can never drift — adding a screen in one
 * place adds it to both.
 */
export interface NavRoute {
  path: string
  /** i18n key under the `nav` namespace. */
  labelKey: string
  icon: Component
}

export const NAV_ROUTES: NavRoute[] = [
  { path: '/projects', labelKey: 'nav.projects', icon: FolderOpen },
  { path: '/pr-inbox', labelKey: 'nav.prInbox', icon: GitPullRequest },
  { path: '/gantt', labelKey: 'nav.gantt', icon: GanttChartSquare },
  { path: '/skills', labelKey: 'nav.skills', icon: Puzzle },
  { path: '/rules', labelKey: 'nav.rules', icon: ScrollText },
  { path: '/mcp', labelKey: 'nav.mcp', icon: Server },
  { path: '/servers', labelKey: 'nav.servers', icon: HardDrive },
  { path: '/stats', labelKey: 'nav.stats', icon: BarChart3 },
  { path: '/work-digest', labelKey: 'nav.digest', icon: CalendarClock },
  { path: '/calendar', labelKey: 'nav.calendar', icon: CalendarDays },
  { path: '/settings', labelKey: 'nav.settings', icon: Settings },
  { path: '/about', labelKey: 'nav.about', icon: Info },
]
