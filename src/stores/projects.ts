import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'
import type { GroupApplyOutcome, GroupDisableOutcome, GroupKind, ProjectGroupState } from '@/stores/groups'
import type { ApplyOneOutcome } from '@/lib/savedWithoutFolder'

export interface Project {
  id: string
  name: string
  path: string
  github_owner: string | null
  github_repo: string | null
  created_at: string
  /** Default GitHub account (one of `github_account_ids`). */
  github_account_id: string | null
  /** GitHub accounts enabled for the project; runs may pick any per command. */
  github_account_ids: string[]
  /** Default GitLab account (one of `gitlab_account_ids`). */
  gitlab_account_id: string | null
  gitlab_account_ids: string[]
  /** Default AWS account (one of `aws_account_ids`). */
  aws_account_id: string | null
  /** AWS accounts enabled for the project; runs may pick any per command. */
  aws_account_ids: string[]
  /** Default Azure account (one of `azure_account_ids`). */
  azure_account_id: string | null
  azure_account_ids: string[]
  claude_account_id: string | null
  /** GitHub Project V2 board enabled on the project (configured under a GitHub account). */
  github_board_id: string | null
  /** URL / field mappings of the enabled board (read-only, from the board). */
  github_project_board_url: string | null
  github_project_field_mappings: string | null
  run_count: number
  last_used_at: string | null
  position: number
}

export interface Repo {
  id: string
  project_id: string
  name: string
  path: string
  github_owner: string | null
  github_repo: string | null
  created_at: string
  // Git host provider; defaults to 'github' for legacy repos (BR-001).
  provider: 'github' | 'gitlab'
  gitlab_project_path: string | null
  gitlab_project_id: number | null
}

export interface AppliedSkill {
  skill_id: string
  skill_name: string
  skill_description: string
  target: 'claude' | 'codex' | 'both'
  has_claude: boolean
  has_codex: boolean
  applied_at: string
  /** Switched on for this project directly. Independent of `from_group`. */
  manual: boolean
  /** Provided by a group the project has enabled. Either link alone keeps the skill applied. */
  from_group: boolean
}

export interface DetectedRepo {
  name: string
  path: string
  github_owner: string | null
  github_repo: string | null
  /** Provider read off the `origin` remote; 'github' when there's nothing to go on. */
  provider: 'github' | 'gitlab'
  gitlab_project_path: string | null
}

export interface DetectedProjectInfo {
  name: string
  path: string
  repos: DetectedRepo[]
}

export interface SyncConflict {
  id: string
  project_id: string
  skill_id: string
  skill_name: string
  project_name: string
  engine: 'claude' | 'codex'
  detected_at: string
  local_hash: string
  source_hash: string
  resolved: boolean
}

export interface AppliedRule {
  rule_id: string
  rule_name: string
  rule_description: string
  target: 'claude' | 'codex' | 'both'
  has_claude: boolean
  has_codex: boolean
  applied_at: string
  /** Switched on for this project directly. Independent of `from_group`. */
  manual: boolean
  /** Provided by a group the project has enabled. Either link alone keeps the rule applied. */
  from_group: boolean
  /** Codex gets this rule as a skill in `.agents/skills` instead of an AGENTS.md block. */
  codex_skill: boolean
}

/** Read-only findings about how Claude / Codex load this project's instructions. */
export type InstructionWarning =
  | { code: 'agents_md_too_large'; size: number; limit: number }
  | { code: 'claude_reads_agents_md' }
  | { code: 'claude_md_imports_agents_md'; path: string }
  | { code: 'codex_reads_claude_md'; config: string }
  | { code: 'links_not_ignored'; dir: string }
  | { code: 'legacy_copies_backed_up'; dir: string; batches: number }
  | { code: 'tracked_by_git'; paths: string[] }

export interface RuleSyncConflict {
  id: string
  project_id: string
  rule_id: string
  rule_name: string
  project_name: string
  engine: 'claude' | 'codex'
  detected_at: string
  local_hash: string
  source_hash: string
  resolved: boolean
}

export const useProjectsStore = defineStore('projects', () => {
  const projects = ref<Project[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)
  const conflicts = ref<SyncConflict[]>([])
  const ruleConflicts = ref<RuleSyncConflict[]>([])

  async function fetchProjects() {
    loading.value = true
    error.value = null
    try {
      projects.value = await invoke<Project[]>('list_projects')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  /** Move the project at `from` to `to` (drag-and-drop reorder), then persist. */
  async function reorder(from: number, to: number) {
    if (from === to) return
    if (from < 0 || from >= projects.value.length) return
    if (to < 0 || to >= projects.value.length) return
    const snapshot = projects.value.slice()
    const [moved] = projects.value.splice(from, 1)
    projects.value.splice(to, 0, moved)
    try {
      await invoke('reorder_projects', { ids: projects.value.map(p => p.id) })
    } catch (e) {
      error.value = String(e)
      projects.value = snapshot
    }
  }

  async function detectProjectInfo(path: string): Promise<DetectedProjectInfo> {
    return invoke<DetectedProjectInfo>('detect_project_info', { path })
  }

  async function addProject(payload: {
    path: string
    name?: string
    repos?: Array<{
      name: string
      path: string
      github_owner?: string
      github_repo?: string
      provider?: 'github' | 'gitlab'
      gitlab_project_path?: string | null
      gitlab_project_id?: number | null
    }>
  }): Promise<Project> {
    const project = await invoke<Project>('add_project', { payload })
    await fetchProjects()
    return project
  }

  async function removeProject(id: string): Promise<void> {
    await invoke('remove_project', { id })
    await fetchProjects()
  }

  async function updateProject(payload: { id: string; name: string }): Promise<Project> {
    const project = await invoke<Project>('update_project', { id: payload.id, name: payload.name })
    await fetchProjects()
    return project
  }

  /** Enable a GitHub board on the project (replacing any other), or disable with null. */
  async function setProjectBoard(project_id: string, board_id: string | null): Promise<void> {
    await invoke('set_project_github_board', { projectId: project_id, boardId: board_id })
    await fetchProjects()
  }

  async function listRepos(project_id: string): Promise<Repo[]> {
    return invoke<Repo[]>('list_repos', { projectId: project_id })
  }

  async function addRepo(payload: {
    project_id: string
    name: string
    path: string
    github_owner?: string
    github_repo?: string
    provider?: 'github' | 'gitlab'
    gitlab_project_path?: string | null
    gitlab_project_id?: number | null
  }): Promise<Repo> {
    return invoke<Repo>('add_repo', {
      projectId: payload.project_id,
      name: payload.name,
      path: payload.path,
      githubOwner: payload.github_owner,
      githubRepo: payload.github_repo,
      provider: payload.provider ?? null,
      gitlabProjectPath: payload.gitlab_project_path ?? null,
      gitlabProjectId: payload.gitlab_project_id ?? null,
    })
  }

  async function updateRepo(payload: {
    id: string
    name: string
    github_owner: string | null
    github_repo: string | null
    provider?: 'github' | 'gitlab'
    gitlab_project_path?: string | null
    gitlab_project_id?: number | null
  }): Promise<Repo> {
    return invoke<Repo>('update_repo', {
      id: payload.id,
      name: payload.name,
      githubOwner: payload.github_owner,
      githubRepo: payload.github_repo,
      provider: payload.provider ?? null,
      gitlabProjectPath: payload.gitlab_project_path ?? null,
      gitlabProjectId: payload.gitlab_project_id ?? null,
    })
  }

  async function removeRepo(id: string): Promise<void> {
    await invoke('remove_repo', { id })
  }

  async function getAppliedSkills(project_id: string): Promise<AppliedSkill[]> {
    return invoke<AppliedSkill[]>('get_applied_skills', { projectId: project_id })
  }

  async function applySkill(project_id: string, skill_id: string): Promise<ApplyOneOutcome> {
    return invoke<ApplyOneOutcome>('apply_skill', { projectId: project_id, skillId: skill_id })
  }

  async function removeSkillFromProject(project_id: string, skill_id: string): Promise<void> {
    await invoke('remove_skill_from_project', { projectId: project_id, skillId: skill_id })
  }

  async function setProjectAccount(project_id: string, account_id: string | null): Promise<void> {
    await invoke('set_project_github_account', { projectId: project_id, accountId: account_id })
    await fetchProjects()
  }

  /** Enable / disable one GitHub or GitLab account for a project. */
  async function setProjectGitAccountEnabled(
    project_id: string,
    provider: 'github' | 'gitlab',
    account_id: string,
    enabled: boolean,
  ): Promise<void> {
    await invoke('set_project_git_account_enabled', { projectId: project_id, provider, accountId: account_id, enabled })
    await fetchProjects()
  }

  async function setProjectGitlabAccount(project_id: string, account_id: string | null): Promise<void> {
    await invoke('set_project_gitlab_account', { projectId: project_id, accountId: account_id })
    await fetchProjects()
  }

  async function setProjectAwsAccount(project_id: string, account_id: string | null): Promise<void> {
    await invoke('set_project_aws_account', { projectId: project_id, accountId: account_id })
    await fetchProjects()
  }

  async function setProjectAzureAccount(project_id: string, account_id: string | null): Promise<void> {
    await invoke('set_project_azure_account', { projectId: project_id, accountId: account_id })
    await fetchProjects()
  }

  /** Enable / disable one AWS or Azure account for a project. */
  async function setProjectCloudAccountEnabled(
    project_id: string,
    provider: 'aws' | 'azure',
    account_id: string,
    enabled: boolean,
  ): Promise<void> {
    await invoke('set_project_cloud_account_enabled', { projectId: project_id, provider, accountId: account_id, enabled })
    await fetchProjects()
  }

  async function setProjectClaudeAccount(project_id: string, account_id: string | null): Promise<void> {
    await invoke('set_project_claude_account', { projectId: project_id, accountId: account_id })
    await fetchProjects()
  }

  async function fetchConflicts(): Promise<void> {
    conflicts.value = await invoke<SyncConflict[]>('list_sync_conflicts')
  }

  async function resolveConflict(conflict_id: string, overwrite: boolean): Promise<void> {
    await invoke('resolve_sync_conflict', { conflictId: conflict_id, overwrite })
    await fetchConflicts()
  }

  async function openInVscode(path: string, file?: string): Promise<void> {
    await invoke('open_in_vscode', { path, file: file ?? null })
  }

  async function openInFolder(path: string): Promise<void> {
    await invoke('open_in_folder', { path })
  }

  async function openInTerminal(path: string, terminalApp: string): Promise<void> {
    await invoke('open_in_terminal', { path, terminalApp })
  }

  async function getAppliedRules(project_id: string): Promise<AppliedRule[]> {
    return invoke<AppliedRule[]>('get_applied_rules', { projectId: project_id })
  }

  async function applyRule(project_id: string, rule_id: string): Promise<ApplyOneOutcome> {
    return invoke<ApplyOneOutcome>('apply_rule', { projectId: project_id, ruleId: rule_id })
  }

  async function removeRuleFromProject(project_id: string, rule_id: string): Promise<void> {
    await invoke('remove_rule_from_project', { projectId: project_id, ruleId: rule_id })
  }

  async function getInstructionWarnings(project_id: string): Promise<InstructionWarning[]> {
    return invoke<InstructionWarning[]>('get_instruction_warnings', { projectId: project_id })
  }

  async function fetchRuleConflicts(): Promise<void> {
    ruleConflicts.value = await invoke<RuleSyncConflict[]>('list_rule_sync_conflicts')
  }

  async function resolveRuleConflict(conflict_id: string, overwrite: boolean): Promise<void> {
    await invoke('resolve_rule_sync_conflict', { conflictId: conflict_id, overwrite })
    await fetchRuleConflicts()
  }

  // ---- Skill / rule groups -------------------------------------------------

  async function getProjectGroups(kind: GroupKind, project_id: string): Promise<ProjectGroupState[]> {
    return invoke<ProjectGroupState[]>('get_project_groups', { kind, projectId: project_id })
  }

  async function enableGroup(kind: GroupKind, project_id: string, group_id: string): Promise<GroupApplyOutcome> {
    return invoke<GroupApplyOutcome>('enable_group_for_project', { kind, projectId: project_id, groupId: group_id })
  }

  async function disableGroup(kind: GroupKind, project_id: string, group_id: string): Promise<GroupDisableOutcome> {
    return invoke<GroupDisableOutcome>('disable_group_for_project', { kind, projectId: project_id, groupId: group_id })
  }

  return {
    projects, loading, error, conflicts, ruleConflicts,
    fetchProjects, reorder, detectProjectInfo, addProject, removeProject, updateProject,
    setProjectBoard, listRepos, addRepo, updateRepo, removeRepo,
    getAppliedSkills, applySkill, removeSkillFromProject,
    setProjectAccount, setProjectGitAccountEnabled, setProjectGitlabAccount, setProjectAwsAccount, setProjectAzureAccount, setProjectCloudAccountEnabled, setProjectClaudeAccount, fetchConflicts, resolveConflict,
    openInVscode, openInFolder, openInTerminal,
    getAppliedRules, applyRule, removeRuleFromProject,
    fetchRuleConflicts, resolveRuleConflict, getInstructionWarnings,
    getProjectGroups, enableGroup, disableGroup,
  }
})
