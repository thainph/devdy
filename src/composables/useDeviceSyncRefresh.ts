// After a device pull sync overwrote local data: refetch every store that mirrors a synced
// category in this window, and broadcast the existing change events so other webviews
// (item list, Focus, Slack threads windows) refetch too. Failures are ignored — a stale
// list is better than a failed sync report.
import { emit } from '@tauri-apps/api/event'
import { setLocale } from '@/i18n'
import { ITEM_CHANGED } from '@/lib/itemWindow'
import { FOCUS_CHANGED } from '@/lib/focus'
import type { SyncCategories } from '@/lib/deviceSync'
import { useTodosStore } from '@/stores/todos'
import { useNotesStore } from '@/stores/notes'
import { useFocusStore } from '@/stores/focus'
import { SLACK_THREADS_CHANGED, useSlackThreadsStore } from '@/stores/slackThreads'
import { useSkillsStore } from '@/stores/skills'
import { useRulesStore } from '@/stores/rules'
import { useRuleGroupsStore, useSkillGroupsStore } from '@/stores/groups'
import { useProjectsStore } from '@/stores/projects'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useMcpServersStore } from '@/stores/mcpServers'
import { useServersStore } from '@/stores/servers'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { useGitlabAccountsStore } from '@/stores/gitlabAccounts'
import { useAwsAccountsStore } from '@/stores/awsAccounts'
import { useAzureAccountsStore } from '@/stores/azureAccounts'
import { useClaudeAccountsStore } from '@/stores/claudeAccounts'
import { useGoogleCalendarStore } from '@/stores/googleCalendar'

export function useDeviceSyncRefresh() {
  const todos = useTodosStore()
  const notes = useNotesStore()
  const focus = useFocusStore()
  const slack = useSlackThreadsStore()
  const skills = useSkillsStore()
  const rules = useRulesStore()
  const skillGroups = useSkillGroupsStore()
  const ruleGroups = useRuleGroupsStore()
  const projects = useProjectsStore()
  const appSettings = useAppSettingsStore()
  const mcp = useMcpServersStore()
  const servers = useServersStore()
  const github = useGithubAccountsStore()
  const gitlab = useGitlabAccountsStore()
  const aws = useAwsAccountsStore()
  const azure = useAzureAccountsStore()
  const claude = useClaudeAccountsStore()
  const google = useGoogleCalendarStore()

  async function refreshAfterSync(c: SyncCategories) {
    const jobs: Promise<unknown>[] = []
    const events: [string, unknown][] = []
    if (c.todos || c.notes) {
      if (c.todos) jobs.push(todos.fetchTodos(), focus.load())
      if (c.notes) jobs.push(notes.fetchNotes())
      events.push([ITEM_CHANGED, {}], [FOCUS_CHANGED, {}])
    }
    if (c.slackThreads) {
      jobs.push(slack.fetchList(), slack.reloadView())
      events.push([SLACK_THREADS_CHANGED, {}])
    }
    if (c.skillsRules) {
      jobs.push(skills.fetchSkills(), rules.fetchRules(), skillGroups.fetchGroups(), ruleGroups.fetchGroups())
    }
    if (c.projects || c.skillsRules) {
      jobs.push(projects.fetchProjects(), projects.fetchConflicts(), projects.fetchRuleConflicts())
    }
    if (c.settings) {
      jobs.push(appSettings.refresh().then(() => setLocale(appSettings.settings?.language ?? 'en')))
    }
    if (c.mcpServers) jobs.push(mcp.fetchServers())
    if (c.vpsServers) jobs.push(servers.fetchServers())
    if (c.accounts) {
      jobs.push(github.fetch(), gitlab.fetch(), aws.fetch(), azure.fetch(), claude.fetch(), google.fetchAccounts())
    }
    for (const [name, payload] of events) jobs.push(emit(name, payload))
    await Promise.allSettled(jobs)
  }

  return { refreshAfterSync }
}
