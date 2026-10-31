import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router'
import SkillsView from '../views/SkillsView.vue'
import RulesView from '../views/RulesView.vue'
import McpServersView from '../views/McpServersView.vue'
import ServersView from '../views/ServersView.vue'
import ProjectsView from '../views/ProjectsView.vue'
import PrInboxView from '../views/PrInboxView.vue'
import { openPrInboxWindow } from '../lib/prInboxWindow'
import { openCalendarWindow } from '../lib/calendarWindow'
import { openGanttWindow } from '../lib/ganttWindow'
import { openFocusWindow } from '../lib/focusWindow'
import IssuesGanttView from '../views/IssuesGanttView.vue'
import ProjectDetailView from '../views/ProjectDetailView.vue'
import RunView from '../views/RunView.vue'
import SettingsView from '../views/SettingsView.vue'
import StatsView from '../views/StatsView.vue'
import CalendarView from '../views/CalendarView.vue'
import FocusView from '../views/FocusView.vue'
import AboutView from '../views/AboutView.vue'
import BackgroundProcessesView from '../views/BackgroundProcessesView.vue'

// Skills / Rules / MCP / Servers edit in their list's detail pane now; the old
// editor URLs redirect there so stale links keep working.
function editorRedirects(base: string): RouteRecordRaw[] {
  return [
    { path: `${base}/new`, redirect: { path: base, query: { new: '1' } } },
    { path: `${base}/:id/edit`, redirect: (to) => ({ path: base, query: { id: String(to.params.id) } }) },
  ]
}

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/projects' },
    { path: '/skills', name: 'skills', component: SkillsView },
    ...editorRedirects('/skills'),
    { path: '/rules', name: 'rules', component: RulesView },
    ...editorRedirects('/rules'),
    { path: '/mcp', name: 'mcp', component: McpServersView },
    ...editorRedirects('/mcp'),
    { path: '/servers', name: 'servers', component: ServersView },
    ...editorRedirects('/servers'),
    { path: '/projects', name: 'projects', component: ProjectsView },
    // The PR Inbox lives in its own window (lib/prInboxWindow), like the Slack
    // threads window; the route only stays so old links / navigation open it.
    // PrInboxView is never rendered here — the guard always diverts.
    {
      path: '/pr-inbox',
      name: 'pr-inbox',
      component: PrInboxView,
      beforeEnter: (_to, from) => {
        void openPrInboxWindow()
        // Stay where we are; on a cold start (nothing matched yet) land on the default screen.
        return from.matched.length > 0 ? false : '/projects'
      },
    },
    // Gantt and Focus too (lib/ganttWindow, lib/focusWindow). `?project=` deep
    // links (Projects screen) carry over to the Gantt window.
    {
      path: '/gantt',
      name: 'gantt',
      component: IssuesGanttView,
      beforeEnter: (to, from) => {
        void openGanttWindow(typeof to.query.project === 'string' ? to.query.project : undefined)
        return from.matched.length > 0 ? false : '/projects'
      },
    },
    {
      path: '/focus',
      name: 'focus',
      component: FocusView,
      beforeEnter: (_to, from) => {
        void openFocusWindow()
        return from.matched.length > 0 ? false : '/projects'
      },
    },
    { path: '/projects/:projectId', name: 'project-run', component: RunView },
    { path: '/projects/:projectId/run/:runId', name: 'project-run-detail', component: RunView },
    { path: '/projects/:projectId/settings', name: 'project-settings', component: ProjectDetailView },
    { path: '/stats', name: 'stats', component: StatsView },
    // Same for the Calendar (lib/calendarWindow): CalendarView is rendered only
    // in that window; the route diverts to it.
    {
      path: '/calendar',
      name: 'calendar',
      component: CalendarView,
      beforeEnter: (_to, from) => {
        void openCalendarWindow()
        return from.matched.length > 0 ? false : '/projects'
      },
    },
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
