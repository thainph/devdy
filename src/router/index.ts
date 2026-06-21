import { createRouter, createWebHistory } from 'vue-router'
import SkillsView from '../views/SkillsView.vue'
import SkillEditorView from '../views/SkillEditorView.vue'
import RulesView from '../views/RulesView.vue'
import RuleEditorView from '../views/RuleEditorView.vue'
import ProjectsView from '../views/ProjectsView.vue'
import ProjectDetailView from '../views/ProjectDetailView.vue'
import RunView from '../views/RunView.vue'
import SettingsView from '../views/SettingsView.vue'
import StatsView from '../views/StatsView.vue'
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
    { path: '/projects', name: 'projects', component: ProjectsView },
    { path: '/projects/:projectId', name: 'project-run', component: RunView },
    { path: '/projects/:projectId/run/:runId', name: 'project-run-detail', component: RunView },
    { path: '/projects/:projectId/settings', name: 'project-settings', component: ProjectDetailView },
    { path: '/stats', name: 'stats', component: StatsView },
    { path: '/settings', name: 'settings', component: SettingsView },
    { path: '/about', name: 'about', component: AboutView },
  ],
})

export default router
