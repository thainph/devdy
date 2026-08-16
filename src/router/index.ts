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
import ProjectDetailView from '../views/ProjectDetailView.vue'
import ProjectIssuesView from '../views/ProjectIssuesView.vue'
import RunView from '../views/RunView.vue'
import SettingsView from '../views/SettingsView.vue'
import StatsView from '../views/StatsView.vue'
import TodosView from '../views/TodosView.vue'
import NotesView from '../views/NotesView.vue'
import WorkDigestView from '../views/WorkDigestView.vue'
import CalendarView from '../views/CalendarView.vue'
import AboutView from '../views/AboutView.vue'

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
    { path: '/pr-inbox', name: 'pr-inbox', component: PrInboxView },
    { path: '/projects/:projectId', name: 'project-run', component: RunView },
    { path: '/projects/:projectId/run/:runId', name: 'project-run-detail', component: RunView },
    { path: '/projects/:projectId/settings', name: 'project-settings', component: ProjectDetailView },
    { path: '/projects/:projectId/issues', name: 'project-issues', component: ProjectIssuesView },
    { path: '/todos', name: 'todos', component: TodosView },
    { path: '/notes', name: 'notes', component: NotesView },
    { path: '/stats', name: 'stats', component: StatsView },
    { path: '/work-digest', name: 'work-digest', component: WorkDigestView },
    { path: '/calendar', name: 'calendar', component: CalendarView },
    { path: '/settings', name: 'settings', component: SettingsView },
    { path: '/about', name: 'about', component: AboutView },
  ],
})

export default router
