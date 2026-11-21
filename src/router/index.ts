import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router'
import SkillsView from '../views/SkillsView.vue'
import RulesView from '../views/RulesView.vue'
import McpServersView from '../views/McpServersView.vue'
import ServersView from '../views/ServersView.vue'
import ProjectsView from '../views/ProjectsView.vue'
import DashboardView from '../views/DashboardView.vue'
import SurfaceRouteView from '../views/SurfaceRouteView.vue'
import { cleanParams, SURFACE_PATHS, TAB_INTENT, type SurfaceKind } from '../lib/surfaces'
import { openSurfaceWindow, resolvePlacement } from '../lib/surfacePlacement'
import ProjectDetailView from '../views/ProjectDetailView.vue'
import RunView from '../views/RunView.vue'
import SettingsView from '../views/SettingsView.vue'
import StatsView from '../views/StatsView.vue'
import AboutView from '../views/AboutView.vue'
import BackgroundProcessesView from '../views/BackgroundProcessesView.vue'
import { IS_SESSION_WINDOW } from '../lib/windowMode'

// Skills / Rules / MCP / Servers edit in their list's detail pane now; the old
// editor URLs redirect there so stale links keep working.
function editorRedirects(base: string): RouteRecordRaw[] {
  return [
    { path: `${base}/new`, redirect: { path: base, query: { new: '1' } } },
    { path: `${base}/:id/edit`, redirect: (to) => ({ path: base, query: { id: String(to.params.id) } }) },
  ]
}

/**
 * Route of a surface tab. Its guard sends the surface to its own window when
 * that is where it opens now (lib/surfacePlacement) — unless the navigation is
 * explicitly a tab (the TAB_INTENT query flag: switching / docking tabs). A pop-out
 * session window never hosts surfaces: it hands them to the main window.
 */
function surfaceRoute(kind: SurfaceKind, name: string): RouteRecordRaw {
  return {
    path: SURFACE_PATHS[kind],
    name,
    component: SurfaceRouteView,
    props: { surface: kind },
    beforeEnter: async (to, from) => {
      const stay = from.matched.length > 0 ? false : '/dashboard'
      if (IS_SESSION_WINDOW) {
        const { openSurface } = await import('../lib/surfaceOpen')
        void openSurface(kind, to.query)
        return stay
      }
      if (to.query[TAB_INTENT] === '1') return true
      if ((await resolvePlacement(kind)) === 'tab') return true
      void openSurfaceWindow(kind, cleanParams(kind, to.query))
      return stay
    },
  }
}

const router = createRouter({
  history: createWebHistory(),
  routes: [
    // The Runtime Dashboard is the start screen.
    { path: '/', redirect: '/dashboard' },
    { path: '/dashboard', name: 'dashboard', component: DashboardView },
    { path: '/skills', name: 'skills', component: SkillsView },
    ...editorRedirects('/skills'),
    { path: '/rules', name: 'rules', component: RulesView },
    ...editorRedirects('/rules'),
    { path: '/mcp', name: 'mcp', component: McpServersView },
    ...editorRedirects('/mcp'),
    { path: '/servers', name: 'servers', component: ServersView },
    ...editorRedirects('/servers'),
    { path: '/projects', name: 'projects', component: ProjectsView },
    // Surfaces: a tab of this window or their own OS window, per the
    // "Windows & tabs" setting (lib/surfaces). The guard diverts to the window.
    surfaceRoute('mentions', 'github-mentions'),
    surfaceRoute('prInbox', 'pr-inbox'),
    { path: '/gantt', redirect: '/radar' },
    surfaceRoute('radar', 'radar'),
    surfaceRoute('slack', 'slack'),
    surfaceRoute('todos', 'todos'),
    surfaceRoute('notes', 'notes'),
    surfaceRoute('file', 'file'),
    // The Focus board became the Todos list.
    { path: '/focus', redirect: '/todos' },
    { path: '/projects/:projectId', name: 'project-run', component: RunView },
    { path: '/projects/:projectId/run/:runId', name: 'project-run-detail', component: RunView },
    { path: '/projects/:projectId/settings', name: 'project-settings', component: ProjectDetailView },
    { path: '/stats', name: 'stats', component: StatsView },
    surfaceRoute('calendar', 'calendar'),
    { path: '/settings', name: 'settings', component: SettingsView },
    { path: '/processes', name: 'processes', component: BackgroundProcessesView },
    { path: '/about', name: 'about', component: AboutView },
    ...(import.meta.env.DEV
      ? [{
          path: '/dev/cyber-fox',
          name: 'dev-cyber-fox',
          component: () => import('../views/dev/CyberFoxDebugView.vue'),
        }]
      : []),
  ],
})

export default router
