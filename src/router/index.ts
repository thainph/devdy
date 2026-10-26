import { createRouter, createWebHistory } from 'vue-router'
import SkillsView from '../views/SkillsView.vue'
import SkillEditorView from '../views/SkillEditorView.vue'
import RulesView from '../views/RulesView.vue'
import RuleEditorView from '../views/RuleEditorView.vue'
import McpServersView from '../views/McpServersView.vue'
import McpServerEditorView from '../views/McpServerEditorView.vue'
import ServersView from '../views/ServersView.vue'
import ServerEditorView from '../views/ServerEditorView.vue'
import ProjectsView from '../views/ProjectsView.vue'
import PrInboxView from '../views/PrInboxView.vue'
import { openPrInboxWindow } from '../lib/prInboxWindow'
import { openCalendarWindow } from '../lib/calendarWindow'
import { openGanttWindow } from '../lib/ganttWindow'
import { openPulseWindow } from '../lib/pulseWindow'
import IssuesGanttView from '../views/IssuesGanttView.vue'
import ProjectDetailView from '../views/ProjectDetailView.vue'
import RunView from '../views/RunView.vue'
import SettingsView from '../views/SettingsView.vue'
import StatsView from '../views/StatsView.vue'
import CalendarView from '../views/CalendarView.vue'
import PulseView from '../views/PulseView.vue'
import AboutView from '../views/AboutView.vue'
import BackgroundProcessesView from '../views/BackgroundProcessesView.vue'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/projects' },
    { path: '/skills', name: 'skills', component: SkillsView },
    { path: '/skills/new', name: 'skill-new', component: SkillEditorView },
    { path: '/skills/:id/edit', name: 'skill-edit', component: SkillEditorView },
    { path: '/rules', name: 'rules', component: RulesView },
    { path: '/rules/new', name: 'rule-new', component: RuleEditorView },
    { path: '/rules/:id/edit', name: 'rule-edit', component: RuleEditorView },
    { path: '/mcp', name: 'mcp', component: McpServersView },
    { path: '/mcp/new', name: 'mcp-new', component: McpServerEditorView },
    { path: '/mcp/:id/edit', name: 'mcp-edit', component: McpServerEditorView },
    { path: '/servers', name: 'servers', component: ServersView },
    { path: '/servers/new', name: 'server-new', component: ServerEditorView },
    { path: '/servers/:id/edit', name: 'server-edit', component: ServerEditorView },
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
    // Gantt and Pulse too (lib/ganttWindow, lib/pulseWindow). `?project=` deep
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
      path: '/pulse',
      name: 'pulse',
      component: PulseView,
      beforeEnter: (_to, from) => {
        void openPulseWindow()
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
