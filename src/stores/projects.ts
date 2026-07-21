import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export interface Project {
  id: string
  name: string
  path: string
  github_owner: string | null
  github_repo: string | null
  created_at: string
  github_account_id: string | null
  gitlab_account_id: string | null
  aws_account_id: string | null
  run_count: number
  last_used_at: string | null
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
}

export interface DetectedRepo {
  name: string
  path: string
  github_owner: string | null
  github_repo: string | null
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
}

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

  async function detectProjectInfo(path: string): Promise<DetectedProjectInfo> {
    return invoke<DetectedProjectInfo>('detect_project_info', { path })
  }

  async function addProject(payload: {
    path: string
    name?: string
    repos?: Array<{ name: string; path: string; github_owner?: string; github_repo?: string }>
  }): Promise<Project> {
    const project = await invoke<Project>('add_project', { payload })
    await fetchProjects()
    return project
  }

  async function removeProject(id: string): Promise<void> {
    await invoke('remove_project', { id })
    await fetchProjects()
  }

  async function updateProject(payload: {
    id: string
    name: string
  }): Promise<Project> {
    const project = await invoke<Project>('update_project', {
      id: payload.id,
      name: payload.name,
    })
    await fetchProjects()
    return project
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

  async function applySkill(project_id: string, skill_id: string): Promise<void> {
    await invoke('apply_skill', { projectId: project_id, skillId: skill_id })
  }

  async function removeSkillFromProject(project_id: string, skill_id: string): Promise<void> {
    await invoke('remove_skill_from_project', { projectId: project_id, skillId: skill_id })
  }

  async function setProjectAccount(project_id: string, account_id: string | null): Promise<void> {
    await invoke('set_project_github_account', { projectId: project_id, accountId: account_id })
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

  async function applyRule(project_id: string, rule_id: string): Promise<void> {
    await invoke('apply_rule', { projectId: project_id, ruleId: rule_id })
  }

  async function removeRuleFromProject(project_id: string, rule_id: string): Promise<void> {
    await invoke('remove_rule_from_project', { projectId: project_id, ruleId: rule_id })
  }

  async function fetchRuleConflicts(): Promise<void> {
    ruleConflicts.value = await invoke<RuleSyncConflict[]>('list_rule_sync_conflicts')
  }

  async function resolveRuleConflict(conflict_id: string, overwrite: boolean): Promise<void> {
    await invoke('resolve_rule_sync_conflict', { conflictId: conflict_id, overwrite })
    await fetchRuleConflicts()
  }

  return {
    projects, loading, error, conflicts, ruleConflicts,
    fetchProjects, detectProjectInfo, addProject, removeProject, updateProject,
    listRepos, addRepo, updateRepo, removeRepo,
    getAppliedSkills, applySkill, removeSkillFromProject,
    setProjectAccount, setProjectGitlabAccount, setProjectAwsAccount, fetchConflicts, resolveConflict,
    openInVscode, openInFolder, openInTerminal,
    getAppliedRules, applyRule, removeRuleFromProject,
    fetchRuleConflicts, resolveRuleConflict,
  }
})
