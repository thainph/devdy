import type { Component } from 'vue'
import {
  Puzzle,
  ScrollText,
  Server,
  HardDrive,
  FolderOpen,
  GitPullRequest,
  AtSign,
  Radar,
  BarChart3,
  CalendarDays,
  Cpu,
  Settings,
  Info,
  LayoutDashboard,
} from 'lucide-vue-next'
import type { SurfaceKind } from '@/lib/surfaces'

/**
 * The app's top-level destinations, in sidebar order. Shared by the sidebar and
 * the native "Go" menu so the two can never drift — adding a screen in one
 * place adds it to both.
 *
 * Ordered by how often they're used: daily work first, then the agent
 * configuration library, then app-level screens. A divider is drawn wherever
 * `group` changes.
 */
export type NavGroup = 'work' | 'config' | 'app'

export interface NavRoute {
  path: string
  /** i18n key under the `nav` namespace. */
  labelKey: string
  icon: Component
  group: NavGroup
  /**
   * Set for destinations that are surfaces (lib/surfaces): a tab of the main
   * window or their own OS window, per the "Windows & tabs" setting. The
   * sidebar entry and the Go menu item open them through lib/surfaceOpen.
   */
  surface?: SurfaceKind
}

export const NAV_ROUTES: NavRoute[] = [
  { path: '/dashboard', labelKey: 'nav.dashboard', icon: LayoutDashboard, group: 'work' },
  { path: '/projects', labelKey: 'nav.projects', icon: FolderOpen, group: 'work' },
  { path: '/pr-inbox', labelKey: 'nav.prInbox', icon: GitPullRequest, group: 'work', surface: 'prInbox' },
  { path: '/github-mentions', labelKey: 'nav.githubMentions', icon: AtSign, group: 'work', surface: 'mentions' },
  { path: '/calendar', labelKey: 'nav.calendar', icon: CalendarDays, group: 'work', surface: 'calendar' },
  { path: '/radar', labelKey: 'nav.radar', icon: Radar, group: 'work', surface: 'radar' },
  { path: '/skills', labelKey: 'nav.skills', icon: Puzzle, group: 'config' },
  { path: '/rules', labelKey: 'nav.rules', icon: ScrollText, group: 'config' },
  { path: '/mcp', labelKey: 'nav.mcp', icon: Server, group: 'config' },
  { path: '/servers', labelKey: 'nav.servers', icon: HardDrive, group: 'config' },
  { path: '/stats', labelKey: 'nav.stats', icon: BarChart3, group: 'app' },
  { path: '/processes', labelKey: 'nav.processes', icon: Cpu, group: 'app' },
  { path: '/settings', labelKey: 'nav.settings', icon: Settings, group: 'app' },
  { path: '/about', labelKey: 'nav.about', icon: Info, group: 'app' },
]

/**
 * The page (a NAV_ROUTES destination that isn't a surface: Dashboard, Skills,
 * Settings…) a route path shows, or null. Pages are tabs of the main window too.
 */
export function pageRouteFor(path: string): NavRoute | null {
  return NAV_ROUTES.find((r) => !r.surface && r.path === path) ?? null
}

/** True when `NAV_ROUTES[index]` starts a new group (and so gets a divider). */
export function startsNavGroup(index: number): boolean {
  return index > 0 && NAV_ROUTES[index].group !== NAV_ROUTES[index - 1].group
}
