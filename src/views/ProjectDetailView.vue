<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useProjectsStore, type AppliedSkill, type AppliedRule, type Repo } from '@/stores/projects'
import { useSkillsStore } from '@/stores/skills'
import { useRulesStore } from '@/stores/rules'
import { useMcpServersStore, type ProjectMcpServer } from '@/stores/mcpServers'
import { useServersStore, type ProjectServer } from '@/stores/servers'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { useGitlabAccountsStore } from '@/stores/gitlabAccounts'
import {
  Play, AlertTriangle, Puzzle, ScrollText, Server, Github, Gitlab,
  GitMerge, CheckCircle2, XCircle, Trash2, Plus, GitBranch, Code2, Settings, SquareTerminal, FolderOpen,
  Rocket
} from 'lucide-vue-next'
import { Button, Input, Card, Badge, AppSelect } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'

const route = useRoute()
const router = useRouter()
const projectStore = useProjectsStore()
const skillsStore = useSkillsStore()
const rulesStore = useRulesStore()
const mcpStore = useMcpServersStore()
const serversStore = useServersStore()
const appSettings = useAppSettingsStore()
const ghStore = useGithubAccountsStore()
const glStore = useGitlabAccountsStore()
const { confirm } = useConfirm()
const { toast } = useToast()

const projectId = computed(() => route.params.projectId as string)
const project = computed(() => projectStore.projects.find(p => p.id === projectId.value))

const activeTab = ref<'overview' | 'skills' | 'rules' | 'mcp' | 'deploy' | 'github' | 'gitlab' | 'conflicts'>('overview')
const SECTIONS = [
  { id: 'overview', label: 'Overview', icon: Settings },
  { id: 'skills', label: 'Skills', icon: Puzzle },
  { id: 'rules', label: 'Rules', icon: ScrollText },
  { id: 'mcp', label: 'MCP Servers', icon: Server },
  { id: 'deploy', label: 'Deploy', icon: Rocket },
  { id: 'github', label: 'GitHub', icon: Github },
  { id: 'gitlab', label: 'GitLab', icon: Gitlab },
] as const
const projectConflicts = computed(() => projectStore.conflicts.filter(c => c.project_id === projectId.value))
const projectRuleConflicts = computed(() => projectStore.ruleConflicts.filter(c => c.project_id === projectId.value))
const totalConflicts = computed(() => projectConflicts.value.length + projectRuleConflicts.value.length)
const appliedSkills = ref<AppliedSkill[]>([])
const loadingSkills = ref(false)

const targetLabel: Record<string, string> = { claude: 'Claude', codex: 'Codex', both: 'Both' }
const appliedRules = ref<AppliedRule[]>([])
const loadingRules = ref(false)
const togglingRuleId = ref<string | null>(null)
// Unified list of every rule with its per-project applied state, mirroring the
// MCP servers tab (single toggleable list instead of add/applied panels).
const ruleItems = computed(() =>
  rulesStore.rules.map(r => {
    const applied = appliedRules.value.find(a => a.rule_id === r.id)
    return {
      ...r,
      applied: !!applied,
      has_claude: applied?.has_claude ?? false,
      has_codex: applied?.has_codex ?? false,
    }
  })
)

// --- MCP servers (per-project enable/disable) ---
const mcpServers = ref<ProjectMcpServer[]>([])
const loadingMcp = ref(false)
const savingMcp = ref(false)
// Warn (not block) when a remote server is enabled but the default engine is
// Codex, which can't use http/sse transports.
const defaultIsCodex = computed(() => appSettings.settings?.default_engine === 'codex')

async function loadProjectMcpServers() {
  loadingMcp.value = true
  try {
    mcpServers.value = await mcpStore.listForProject(projectId.value)
  } finally {
    loadingMcp.value = false
  }
}

async function handleToggleMcpServer(server: ProjectMcpServer) {
  const next = !server.enabled_for_project
  server.enabled_for_project = next
  savingMcp.value = true
  try {
    const ids = mcpServers.value.filter(s => s.enabled_for_project).map(s => s.id)
    await mcpStore.setForProject(projectId.value, ids)
  } catch (e) {
    server.enabled_for_project = !next
    alert(String(e))
  } finally {
    savingMcp.value = false
  }
}

// --- Deploy: per-project VPS mapping (role-based) ---
const projectServers = ref<ProjectServer[]>([])
const loadingDeploy = ref(false)
const newDeployServerId = ref('')
const newDeployRole = ref('production')
const addingDeploy = ref(false)
const removingDeployKey = ref<string | null>(null)

// Every managed VPS the user can pick from (loaded from the servers store).
const availableServerOptions = computed(() => [
  { value: '', label: 'Select a VPS…' },
  ...serversStore.items.map(s => ({
    value: s.id,
    label: `${s.label} (${s.username}@${s.host}:${s.port})`,
  })),
])

// UI-suggested roles (BR-101: no hard whitelist; the free-typed role is also
// accepted server-side, but the select covers the common cases).
const deployRoleOptions = [
  { value: 'staging', label: 'staging' },
  { value: 'production', label: 'production' },
]

async function loadProjectServers() {
  loadingDeploy.value = true
  try {
    projectServers.value = await serversStore.listForProject(projectId.value)
  } finally {
    loadingDeploy.value = false
  }
}

async function handleAddDeployServer() {
  const serverId = newDeployServerId.value
  if (!serverId) return
  addingDeploy.value = true
  try {
    await serversStore.mapToProject(projectId.value, serverId, newDeployRole.value)
    newDeployServerId.value = ''
    newDeployRole.value = 'production'
    await loadProjectServers()
  } catch (e) {
    alert(String(e))
  } finally {
    addingDeploy.value = false
  }
}

async function handleRemoveDeployServer(server: ProjectServer) {
  const key = `${server.id}:${server.role}`
  removingDeployKey.value = key
  try {
    await serversStore.unmap(projectId.value, server.id, server.role)
    await loadProjectServers()
  } catch (e) {
    alert(String(e))
  } finally {
    removingDeployKey.value = null
  }
}

async function loadAppliedRules() {
  loadingRules.value = true
  try {
    appliedRules.value = await projectStore.getAppliedRules(projectId.value)
  } finally {
    loadingRules.value = false
  }
}

async function handleToggleRule(rule: { id: string; applied: boolean }) {
  togglingRuleId.value = rule.id
  try {
    if (rule.applied) {
      await projectStore.removeRuleFromProject(projectId.value, rule.id)
    } else {
      await projectStore.applyRule(projectId.value, rule.id)
    }
    await loadAppliedRules()
  } catch (e) {
    alert(String(e))
  } finally {
    togglingRuleId.value = null
  }
}

const accountValidation = ref<{ username: string; scopes: string[]; has_repo_scope: boolean } | null>(null)
const validating = ref(false)
const validationError = ref<string | null>(null)

const linkedAccountId = computed(() => project.value?.github_account_id ?? null)
const linkedAccount = computed(() => ghStore.accounts.find(a => a.id === linkedAccountId.value) ?? null)
const accountOptions = computed(() => [
  { value: '', label: 'None' },
  ...ghStore.accounts.map(a => ({
    value: a.id,
    label: a.username ? `${a.label} (@${a.username})` : a.label,
  })),
])

async function handleSelectAccount(accountId: string) {
  accountValidation.value = null
  validationError.value = null
  try {
    await projectStore.setProjectAccount(projectId.value, accountId || null)
  } catch (e) {
    alert(String(e))
  }
}

async function handleValidateAccount() {
  if (!linkedAccountId.value) return
  validating.value = true
  accountValidation.value = null
  validationError.value = null
  try {
    accountValidation.value = await ghStore.validate(linkedAccountId.value)
  } catch (e) {
    validationError.value = String(e)
  } finally {
    validating.value = false
  }
}

// --- GitLab account linking (mirror of GitHub) ---
const gitlabValidation = ref<{ username: string; email: string | null; scopes: string[] } | null>(null)
const validatingGitlab = ref(false)
const gitlabValidationError = ref<string | null>(null)

const linkedGitlabAccountId = computed(() => project.value?.gitlab_account_id ?? null)
const linkedGitlabAccount = computed(
  () => glStore.accounts.find(a => a.id === linkedGitlabAccountId.value) ?? null,
)
const gitlabAccountOptions = computed(() => [
  { value: '', label: 'None' },
  ...glStore.accounts.map(a => ({
    value: a.id,
    label: a.username ? `${a.label} (@${a.username})` : a.label,
  })),
])

async function handleSelectGitlabAccount(accountId: string) {
  gitlabValidation.value = null
  gitlabValidationError.value = null
  try {
    await projectStore.setProjectGitlabAccount(projectId.value, accountId || null)
  } catch (e) {
    alert(String(e))
  }
}

async function handleValidateGitlabAccount() {
  if (!linkedGitlabAccountId.value) return
  validatingGitlab.value = true
  gitlabValidation.value = null
  gitlabValidationError.value = null
  try {
    gitlabValidation.value = await glStore.validate(linkedGitlabAccountId.value)
  } catch (e) {
    gitlabValidationError.value = String(e)
  } finally {
    validatingGitlab.value = false
  }
}

const editName = ref('')
// Guards auto-save so initial population of the edit fields (from `project`
// and `loadRepos`) doesn't trigger a write.
const overviewReady = ref(false)
let saveTimer: ReturnType<typeof setTimeout> | null = null

const repos = ref<Repo[]>([])
const reposLoading = ref(false)
const editingRepo = ref<{
  [id: string]: {
    name: string
    provider: 'github' | 'gitlab'
    github_owner: string
    github_repo: string
    gitlab_project_path: string
    gitlab_project_id: string
  }
}>({})

const providerOptions: { value: 'github' | 'gitlab'; label: string }[] = [
  { value: 'github', label: 'GitHub' },
  { value: 'gitlab', label: 'GitLab' },
]

const newRepoName = ref('')
const newRepoProvider = ref<'github' | 'gitlab'>('github')
const newRepoOwner = ref('')
const newRepoRepo = ref('')
const newRepoGitlabPath = ref('')
const newRepoGitlabId = ref('')
const addingRepo = ref(false)

const togglingSkillId = ref<string | null>(null)
// Unified list of every skill with its per-project applied state (see ruleItems).
const skillItems = computed(() =>
  skillsStore.skills.map(s => {
    const applied = appliedSkills.value.find(a => a.skill_id === s.id)
    return {
      ...s,
      applied: !!applied,
      has_claude: applied?.has_claude ?? false,
      has_codex: applied?.has_codex ?? false,
    }
  })
)

onMounted(async () => {
  if (projectStore.projects.length === 0) {
    await projectStore.fetchProjects()
  }
  if (skillsStore.skills.length === 0) {
    await skillsStore.fetchSkills()
  }
  if (rulesStore.rules.length === 0) {
    await rulesStore.fetchRules()
  }
  if (ghStore.accounts.length === 0) {
    await ghStore.fetch()
  }
  if (glStore.accounts.length === 0) {
    await glStore.fetch()
  }
  await appSettings.ensureLoaded()
  await loadAppliedSkills()
  await loadAppliedRules()
  await loadProjectMcpServers()
  await serversStore.fetchServers()
  await loadProjectServers()
  await projectStore.fetchConflicts()
  await projectStore.fetchRuleConflicts()
  await loadRepos()
  overviewReady.value = true
})

async function loadRepos() {
  reposLoading.value = true
  try {
    repos.value = await projectStore.listRepos(projectId.value)
    editingRepo.value = {}
    for (const r of repos.value) {
      editingRepo.value[r.id] = {
        name: r.name,
        provider: r.provider ?? 'github',
        github_owner: r.github_owner ?? '',
        github_repo: r.github_repo ?? '',
        gitlab_project_path: r.gitlab_project_path ?? '',
        gitlab_project_id: r.gitlab_project_id != null ? String(r.gitlab_project_id) : '',
      }
    }
  } finally {
    reposLoading.value = false
  }
}

watch(project, (p) => {
  if (p) {
    editName.value = p.name
  }
}, { immediate: true })

async function loadAppliedSkills() {
  loadingSkills.value = true
  try {
    appliedSkills.value = await projectStore.getAppliedSkills(projectId.value)
  } finally {
    loadingSkills.value = false
  }
}

async function autoSaveProject() {
  if (!project.value) return
  let changed = false
  try {
    if (
      editName.value.trim() &&
      editName.value !== project.value.name
    ) {
      await projectStore.updateProject({
        id: projectId.value,
        name: editName.value,
      })
      changed = true
    }
    for (const r of repos.value) {
      const edit = editingRepo.value[r.id]
      if (!edit || !edit.name.trim()) continue
      const editProvider = edit.provider ?? 'github'
      const editGitlabId = edit.gitlab_project_id.trim()
        ? Number(edit.gitlab_project_id.trim())
        : null
      if (
        edit.name === r.name &&
        editProvider === (r.provider ?? 'github') &&
        (edit.github_owner || '') === (r.github_owner ?? '') &&
        (edit.github_repo || '') === (r.github_repo ?? '') &&
        (edit.gitlab_project_path || '') === (r.gitlab_project_path ?? '') &&
        editGitlabId === (r.gitlab_project_id ?? null)
      ) continue
      await projectStore.updateRepo({
        id: r.id,
        name: edit.name,
        github_owner: edit.github_owner || null,
        github_repo: edit.github_repo || null,
        provider: editProvider,
        gitlab_project_path: edit.gitlab_project_path || null,
        gitlab_project_id: editGitlabId,
      })
      // Sync the local source row so the next pass sees no diff.
      r.name = edit.name
      r.provider = editProvider
      r.github_owner = edit.github_owner || null
      r.github_repo = edit.github_repo || null
      r.gitlab_project_path = edit.gitlab_project_path || null
      r.gitlab_project_id = editGitlabId
      changed = true
    }
    if (changed) toast.success('Saved')
  } catch (e) {
    toast.error(String(e))
  }
}

function scheduleSave() {
  if (!overviewReady.value) return
  if (saveTimer) clearTimeout(saveTimer)
  saveTimer = setTimeout(autoSaveProject, 500)
}

watch(editName, scheduleSave)
watch(editingRepo, scheduleSave, { deep: true })

async function handleRemoveRepo(id: string) {
  if (!(await confirm({
    title: 'Remove repository',
    message: 'Remove this repository from the project?',
    confirmLabel: 'Remove',
  }))) return
  try {
    await projectStore.removeRepo(id)
    await loadRepos()
  } catch (e) {
    alert(String(e))
  }
}

async function handleAddRepo() {
  if (!newRepoName.value.trim()) return
  addingRepo.value = true
  try {
    const isGitlab = newRepoProvider.value === 'gitlab'
    await projectStore.addRepo({
      project_id: projectId.value,
      name: newRepoName.value.trim(),
      path: project.value?.path ?? '',
      provider: newRepoProvider.value,
      github_owner: isGitlab ? undefined : newRepoOwner.value || undefined,
      github_repo: isGitlab ? undefined : newRepoRepo.value || undefined,
      gitlab_project_path: isGitlab ? newRepoGitlabPath.value || null : null,
      gitlab_project_id: isGitlab && newRepoGitlabId.value.trim()
        ? Number(newRepoGitlabId.value.trim())
        : null,
    })
    newRepoName.value = ''
    newRepoProvider.value = 'github'
    newRepoOwner.value = ''
    newRepoRepo.value = ''
    newRepoGitlabPath.value = ''
    newRepoGitlabId.value = ''
    await loadRepos()
  } catch (e) {
    alert(String(e))
  } finally {
    addingRepo.value = false
  }
}

async function handleToggleSkill(skill: { id: string; applied: boolean }) {
  togglingSkillId.value = skill.id
  try {
    if (skill.applied) {
      await projectStore.removeSkillFromProject(projectId.value, skill.id)
    } else {
      await projectStore.applySkill(projectId.value, skill.id)
    }
    await loadAppliedSkills()
  } catch (e) {
    alert(String(e))
  } finally {
    togglingSkillId.value = null
  }
}

async function handleOpenInVscode() {
  if (!project.value) return
  try {
    await projectStore.openInVscode(project.value.path)
  } catch (e) {
    alert(String(e))
  }
}

async function handleOpenInFolder() {
  if (!project.value) return
  try {
    await projectStore.openInFolder(project.value.path)
  } catch (e) {
    alert(String(e))
  }
}

async function handleOpenInTerminal() {
  if (!project.value) return
  try {
    const settings = await invoke<{ terminal_app: string }>('get_settings')
    await projectStore.openInTerminal(project.value.path, settings.terminal_app)
  } catch (e) {
    alert(String(e))
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between gap-3 px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2 min-w-0">
        <h1 class="text-sm font-semibold truncate">{{ project?.name ?? 'Project' }}</h1>
        <span
          v-if="project"
          class="text-[11px] text-muted-foreground font-mono truncate hidden md:inline"
          :title="project.path"
        >{{ project.path }}</span>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <Button
          variant="outline"
          title="Open project folder in VS Code"
          @click="handleOpenInVscode"
        >
          <Code2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          VS Code
        </Button>
        <Button
          variant="outline"
          title="Open project folder"
          @click="handleOpenInFolder"
        >
          <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
          Folder
        </Button>
        <Button
          variant="outline"
          title="Open project folder in terminal"
          @click="handleOpenInTerminal"
        >
          <SquareTerminal class="h-3.5 w-3.5" :stroke-width="1.75" />
          Terminal
        </Button>
        <Button @click="router.push(`/projects/${projectId}`)">
          <Play class="h-3.5 w-3.5" :stroke-width="2" />
          Run AI
        </Button>
      </div>
    </div>

    <div v-if="!project" class="flex-1 flex items-center justify-center text-muted-foreground text-sm">
      Project not found
    </div>

    <div v-else class="flex-1 flex min-h-0">
      <!-- Section nav -->
      <nav class="w-48 shrink-0 border-r border-border/60 p-3 overflow-auto">
        <button
          v-for="s in SECTIONS"
          :key="s.id"
          class="w-full flex items-center gap-2.5 px-2.5 py-2 mb-0.5 text-xs rounded-md transition-colors cursor-pointer text-left"
          :class="activeTab === s.id
            ? 'bg-accent text-foreground font-medium'
            : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'"
          @click="activeTab = s.id"
        >
          <component :is="s.icon" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="truncate">{{ s.label }}</span>
        </button>
        <button
          v-if="totalConflicts > 0"
          class="w-full flex items-center gap-2.5 px-2.5 py-2 mb-0.5 text-xs rounded-md transition-colors cursor-pointer text-left"
          :class="activeTab === 'conflicts'
            ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400 font-medium'
            : 'text-amber-500 hover:bg-amber-500/10'"
          @click="activeTab = 'conflicts'"
        >
          <GitMerge class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="truncate">Conflicts</span>
          <span class="ml-auto text-[10px] tabular-nums">{{ totalConflicts }}</span>
        </button>
      </nav>

      <!-- Active section panel -->
      <div class="flex-1 overflow-auto min-w-0">
        <!-- Conflict banner -->
        <div
          v-if="totalConflicts > 0 && activeTab !== 'conflicts'"
          class="flex items-center justify-between gap-3 mx-6 mt-4 px-4 py-3 bg-amber-500/10 border border-amber-500/20 rounded-lg"
        >
          <div class="flex items-center gap-2.5">
            <AlertTriangle class="h-4 w-4 text-amber-500 shrink-0" :stroke-width="1.75" />
            <div>
              <p class="text-xs font-medium text-amber-600 dark:text-amber-400">Sync conflicts detected</p>
              <p class="text-[10px] text-amber-500/80 mt-0.5">
                {{ totalConflicts }} item(s) have local modifications that conflict with central updates
              </p>
            </div>
          </div>
          <button
            class="text-xs font-medium text-amber-600 dark:text-amber-400 hover:underline cursor-pointer shrink-0"
            @click="activeTab = 'conflicts'"
          >
            Resolve
          </button>
        </div>

        <div class="p-6">

        <!-- Overview tab -->
        <div v-if="activeTab === 'overview'" class="max-w-lg space-y-4">
          <!-- General card -->
          <Card body-class="p-4 space-y-4">
            <template #header>
              <Settings class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">General</span>
            </template>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Project Name</label>
              <Input v-model="editName" size="sm" />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Path</label>
              <p class="text-[11px] text-muted-foreground font-mono break-all">{{ project.path }}</p>
            </div>
          </Card>

          <!-- Repositories card -->
          <Card body-class="p-4 space-y-4">
            <template #header>
              <GitBranch class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Repositories</span>
              <span
                v-if="repos.length"
                class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none"
              >{{ repos.length }}</span>
            </template>

            <div v-if="reposLoading" class="space-y-2">
              <div v-for="i in 2" :key="i" class="h-20 rounded-md bg-muted animate-pulse" />
            </div>

            <template v-else>
              <!-- Existing repos -->
              <div
                v-for="repo in repos"
                :key="repo.id"
                class="border border-border rounded-md p-3 space-y-2"
              >
                <div class="flex items-center gap-2">
                  <Input
                    v-model="editingRepo[repo.id].name"
                    type="text"
                    size="sm"
                    class="flex-1"
                    placeholder="Repo name"
                  />
                  <Button
                    variant="destructive-ghost"
                    size="icon-sm"
                    class="shrink-0"
                    @click="handleRemoveRepo(repo.id)"
                  >
                    <Trash2 class="h-3 w-3" :stroke-width="1.75" />
                  </Button>
                </div>
                <div class="text-[11px] text-muted-foreground font-mono truncate px-0.5">{{ repo.path }}</div>
                <AppSelect
                  size="sm"
                  :model-value="editingRepo[repo.id].provider"
                  :options="providerOptions"
                  @update:model-value="editingRepo[repo.id].provider = $event as 'github' | 'gitlab'"
                />
                <!-- GitHub: owner / repo -->
                <div
                  v-if="editingRepo[repo.id].provider === 'github'"
                  class="flex gap-1.5 items-center"
                >
                  <Input
                    v-model="editingRepo[repo.id].github_owner"
                    type="text"
                    size="sm"
                    placeholder="owner"
                  />
                  <span class="text-muted-foreground text-xs shrink-0">/</span>
                  <Input
                    v-model="editingRepo[repo.id].github_repo"
                    type="text"
                    size="sm"
                    placeholder="repo"
                  />
                </div>
                <!-- GitLab: project path + optional numeric project ID -->
                <div v-else class="space-y-1.5">
                  <Input
                    v-model="editingRepo[repo.id].gitlab_project_path"
                    type="text"
                    size="sm"
                    placeholder="namespace/project"
                  />
                  <Input
                    v-model="editingRepo[repo.id].gitlab_project_id"
                    type="text"
                    inputmode="numeric"
                    size="sm"
                    placeholder="Project ID (optional)"
                  />
                </div>
              </div>

              <!-- Add new repo -->
              <div class="border-t border-border/60 pt-3 space-y-2">
                <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Add repository</div>
                <Input
                  v-model="newRepoName"
                  type="text"
                  size="sm"
                  placeholder="Repo name"
                />
                <AppSelect
                  size="sm"
                  :model-value="newRepoProvider"
                  :options="providerOptions"
                  @update:model-value="newRepoProvider = $event as 'github' | 'gitlab'"
                />
                <!-- GitHub: owner / repo -->
                <div v-if="newRepoProvider === 'github'" class="flex gap-1.5 items-center">
                  <Input
                    v-model="newRepoOwner"
                    type="text"
                    size="sm"
                    placeholder="owner"
                  />
                  <span class="text-muted-foreground text-xs shrink-0">/</span>
                  <Input
                    v-model="newRepoRepo"
                    type="text"
                    size="sm"
                    placeholder="repo"
                  />
                </div>
                <!-- GitLab: project path + optional numeric project ID -->
                <div v-else class="space-y-1.5">
                  <Input
                    v-model="newRepoGitlabPath"
                    type="text"
                    size="sm"
                    placeholder="namespace/project"
                  />
                  <Input
                    v-model="newRepoGitlabId"
                    type="text"
                    inputmode="numeric"
                    size="sm"
                    placeholder="Project ID (optional)"
                  />
                </div>
                <Button
                  :disabled="addingRepo || !newRepoName.trim()"
                  @click="handleAddRepo"
                >
                  <Plus class="h-3.5 w-3.5" :stroke-width="2" />
                  {{ addingRepo ? 'Adding…' : 'Add' }}
                </Button>
              </div>
            </template>
          </Card>

        </div>

        <!-- Skills tab -->
        <div v-if="activeTab === 'skills'" class="max-w-lg space-y-5">
          <Card>
            <template #header>
              <Puzzle class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Skills</span>
              <span
                v-if="appliedSkills.length > 0"
                class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none"
              >{{ appliedSkills.length }}</span>
              <RouterLink to="/skills" class="ml-auto text-[11px] text-primary hover:underline">Manage</RouterLink>
            </template>

            <!-- Loading -->
            <div v-if="loadingSkills" class="divide-y divide-border/50">
              <div v-for="i in 2" :key="i" class="flex items-center gap-3 px-4 py-3">
                <div class="h-4 w-4 rounded bg-muted animate-pulse shrink-0" />
                <div class="flex-1 space-y-1.5">
                  <div class="h-2.5 w-24 bg-muted animate-pulse rounded" />
                  <div class="h-2 w-36 bg-muted animate-pulse rounded" />
                </div>
              </div>
            </div>

            <!-- Empty -->
            <div
              v-else-if="skillItems.length === 0"
              class="px-4 py-6 text-center text-xs text-muted-foreground"
            >
              No skills defined yet.
              <RouterLink to="/skills" class="text-primary hover:underline">Create one</RouterLink>
              to enable it here.
            </div>

            <!-- Skill checklist -->
            <div v-else class="divide-y divide-border/50">
              <button
                v-for="skill in skillItems"
                :key="skill.id"
                type="button"
                class="w-full flex items-center gap-3 px-4 py-3 text-left transition-colors cursor-pointer disabled:opacity-50"
                :class="skill.applied ? 'bg-primary/8 hover:bg-primary/12' : 'hover:bg-accent/60'"
                :disabled="togglingSkillId === skill.id"
                @click="handleToggleSkill(skill)"
              >
                <!-- Checkbox indicator -->
                <div
                  class="shrink-0 flex h-4 w-4 items-center justify-center rounded border transition-colors"
                  :class="skill.applied ? 'bg-primary border-primary' : 'border-border bg-background'"
                >
                  <svg
                    v-if="skill.applied"
                    class="h-2.5 w-2.5 text-primary-foreground"
                    viewBox="0 0 12 12" fill="none"
                  >
                    <path d="M2 6l3 3 5-5" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>
                  </svg>
                </div>
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-1.5 flex-wrap">
                    <p class="text-xs font-medium font-mono truncate">{{ skill.name }}</p>
                    <Badge tone="neutral" size="xs" class="shrink-0 uppercase tracking-wide">
                      {{ targetLabel[skill.target] }}
                    </Badge>
                    <Badge v-if="skill.has_claude" tone="primary" size="xs" class="shrink-0">.claude/skills</Badge>
                    <Badge v-if="skill.has_codex" tone="neutral" size="xs" class="shrink-0">.codex/skills</Badge>
                  </div>
                  <p v-if="skill.description" class="text-[10px] text-muted-foreground truncate mt-0.5">{{ skill.description }}</p>
                </div>
              </button>
            </div>
          </Card>
        </div>

        <!-- Rules tab -->
        <div v-if="activeTab === 'rules'" class="max-w-lg space-y-5">
          <Card>
            <template #header>
              <ScrollText class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Rules</span>
              <span
                v-if="appliedRules.length > 0"
                class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none"
              >{{ appliedRules.length }}</span>
              <RouterLink to="/rules" class="ml-auto text-[11px] text-primary hover:underline">Manage</RouterLink>
            </template>

            <!-- Loading -->
            <div v-if="loadingRules" class="divide-y divide-border/50">
              <div v-for="i in 2" :key="i" class="flex items-center gap-3 px-4 py-3">
                <div class="h-4 w-4 rounded bg-muted animate-pulse shrink-0" />
                <div class="flex-1 space-y-1.5">
                  <div class="h-2.5 w-24 bg-muted animate-pulse rounded" />
                  <div class="h-2 w-36 bg-muted animate-pulse rounded" />
                </div>
              </div>
            </div>

            <!-- Empty -->
            <div
              v-else-if="ruleItems.length === 0"
              class="px-4 py-6 text-center text-xs text-muted-foreground"
            >
              No rules defined yet.
              <RouterLink to="/rules" class="text-primary hover:underline">Create one</RouterLink>
              to enable it here.
            </div>

            <!-- Rule checklist -->
            <div v-else class="divide-y divide-border/50">
              <button
                v-for="rule in ruleItems"
                :key="rule.id"
                type="button"
                class="w-full flex items-center gap-3 px-4 py-3 text-left transition-colors cursor-pointer disabled:opacity-50"
                :class="rule.applied ? 'bg-primary/8 hover:bg-primary/12' : 'hover:bg-accent/60'"
                :disabled="togglingRuleId === rule.id"
                @click="handleToggleRule(rule)"
              >
                <!-- Checkbox indicator -->
                <div
                  class="shrink-0 flex h-4 w-4 items-center justify-center rounded border transition-colors"
                  :class="rule.applied ? 'bg-primary border-primary' : 'border-border bg-background'"
                >
                  <svg
                    v-if="rule.applied"
                    class="h-2.5 w-2.5 text-primary-foreground"
                    viewBox="0 0 12 12" fill="none"
                  >
                    <path d="M2 6l3 3 5-5" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>
                  </svg>
                </div>
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-1.5 flex-wrap">
                    <p class="text-xs font-medium font-mono truncate">{{ rule.name }}</p>
                    <Badge tone="neutral" size="xs" class="shrink-0 uppercase tracking-wide">
                      {{ targetLabel[rule.target] }}
                    </Badge>
                    <Badge v-if="rule.has_claude" tone="primary" size="xs" class="shrink-0">.claude/rules</Badge>
                    <Badge v-if="rule.has_codex" tone="neutral" size="xs" class="shrink-0">AGENTS.md</Badge>
                  </div>
                  <p v-if="rule.description" class="text-[10px] text-muted-foreground truncate mt-0.5">{{ rule.description }}</p>
                </div>
              </button>
            </div>
          </Card>
        </div>

        <!-- MCP Servers tab -->
        <div v-if="activeTab === 'mcp'" class="max-w-lg space-y-5">
          <Card>
            <template #header>
              <Server class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">MCP Servers</span>
              <span
                v-if="mcpServers.filter(s => s.enabled_for_project).length > 0"
                class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none"
              >{{ mcpServers.filter(s => s.enabled_for_project).length }}</span>
              <RouterLink to="/mcp" class="ml-auto text-[11px] text-primary hover:underline">Manage</RouterLink>
            </template>

            <!-- Codex warning banner -->
            <div
              v-if="defaultIsCodex && mcpServers.some(s => (s.transport === 'http' || s.transport === 'sse') && s.enabled_for_project)"
              class="mx-4 mt-4 flex items-start gap-2 px-3 py-2 bg-amber-500/10 border border-amber-500/20 rounded-md"
            >
              <AlertTriangle class="h-3.5 w-3.5 text-amber-500 mt-0.5 shrink-0" :stroke-width="1.75" />
              <p class="text-[11px] text-amber-600 dark:text-amber-400 leading-relaxed">
                The default engine is Codex, which only supports stdio servers. Enabled http/sse servers will be skipped on Codex runs.
              </p>
            </div>

            <!-- Loading -->
            <div v-if="loadingMcp" class="divide-y divide-border/50">
              <div v-for="i in 2" :key="i" class="flex items-center gap-3 px-4 py-3">
                <div class="h-4 w-4 rounded bg-muted animate-pulse shrink-0" />
                <div class="flex-1 space-y-1.5">
                  <div class="h-2.5 w-24 bg-muted animate-pulse rounded" />
                  <div class="h-2 w-36 bg-muted animate-pulse rounded" />
                </div>
              </div>
            </div>

            <!-- Empty -->
            <div
              v-else-if="mcpServers.length === 0"
              class="px-4 py-6 text-center text-xs text-muted-foreground"
            >
              No MCP servers defined yet.
              <RouterLink to="/mcp" class="text-primary hover:underline">Create one</RouterLink>
              to enable it here.
            </div>

            <!-- Server checklist -->
            <div v-else class="divide-y divide-border/50">
              <button
                v-for="server in mcpServers"
                :key="server.id"
                type="button"
                class="w-full flex items-center gap-3 px-4 py-3 text-left transition-colors cursor-pointer disabled:opacity-50"
                :class="server.enabled_for_project ? 'bg-primary/8 hover:bg-primary/12' : 'hover:bg-accent/60'"
                :disabled="savingMcp"
                @click="handleToggleMcpServer(server)"
              >
                <!-- Checkbox indicator -->
                <div
                  class="shrink-0 flex h-4 w-4 items-center justify-center rounded border transition-colors"
                  :class="server.enabled_for_project ? 'bg-primary border-primary' : 'border-border bg-background'"
                >
                  <svg
                    v-if="server.enabled_for_project"
                    class="h-2.5 w-2.5 text-primary-foreground"
                    viewBox="0 0 12 12" fill="none"
                  >
                    <path d="M2 6l3 3 5-5" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>
                  </svg>
                </div>
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-1.5 flex-wrap">
                    <p class="text-xs font-medium font-mono truncate">{{ server.name }}</p>
                    <Badge tone="neutral" size="xs" class="shrink-0 uppercase tracking-wide">{{ server.transport }}</Badge>
                    <Badge v-if="!server.enabled" tone="neutral" size="xs" class="shrink-0">disabled</Badge>
                    <Badge
                      v-if="(server.transport === 'http' || server.transport === 'sse') && defaultIsCodex"
                      tone="warning"
                      size="xs"
                      class="shrink-0"
                      title="Codex can't use http/sse servers; this will be skipped on Codex runs."
                    >Claude only</Badge>
                  </div>
                  <p v-if="server.description" class="text-[10px] text-muted-foreground truncate mt-0.5">{{ server.description }}</p>
                </div>
              </button>
            </div>
          </Card>
        </div>

        <!-- Deploy tab (per-project VPS mapping by role) -->
        <div v-if="activeTab === 'deploy'" class="max-w-lg space-y-5">
          <!-- Assigned servers -->
          <Card>
            <template #header>
              <Rocket class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Assigned VPS</span>
              <span
                v-if="projectServers.length > 0"
                class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none"
              >{{ projectServers.length }}</span>
              <RouterLink to="/servers" class="ml-auto text-[11px] text-primary hover:underline">Manage</RouterLink>
            </template>

            <!-- Loading -->
            <div v-if="loadingDeploy" class="divide-y divide-border/50">
              <div v-for="i in 2" :key="i" class="flex items-center gap-3 px-4 py-3">
                <div class="h-4 w-4 rounded bg-muted animate-pulse shrink-0" />
                <div class="flex-1 space-y-1.5">
                  <div class="h-2.5 w-24 bg-muted animate-pulse rounded" />
                  <div class="h-2 w-36 bg-muted animate-pulse rounded" />
                </div>
              </div>
            </div>

            <!-- Empty -->
            <div
              v-else-if="projectServers.length === 0"
              class="px-4 py-6 text-center text-xs text-muted-foreground"
            >
              No VPS assigned to this project yet. Add one below to enable deployments.
            </div>

            <!-- Assigned list -->
            <div v-else class="divide-y divide-border/50">
              <div
                v-for="server in projectServers"
                :key="`${server.id}:${server.role}`"
                class="px-4 py-3"
              >
                <div class="flex items-center gap-3">
                  <div class="flex-1 min-w-0">
                    <div class="flex items-center gap-1.5 flex-wrap">
                      <p class="text-xs font-medium truncate">{{ server.label }}</p>
                      <Badge tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">{{ server.role }}</Badge>
                      <Badge v-if="server.has_passphrase" tone="neutral" size="xs" class="shrink-0">passphrase</Badge>
                    </div>
                    <p class="text-[10px] text-muted-foreground font-mono truncate mt-0.5">
                      {{ server.username }}@{{ server.host }}:{{ server.port }}
                    </p>
                  </div>
                  <Button
                    variant="destructive-ghost"
                    size="icon-sm"
                    class="shrink-0"
                    :disabled="removingDeployKey === `${server.id}:${server.role}`"
                    title="Remove this mapping"
                    @click="handleRemoveDeployServer(server)"
                  >
                    <Trash2 class="h-3 w-3" :stroke-width="1.75" />
                  </Button>
                </div>
              </div>
            </div>
          </Card>

          <!-- Add mapping -->
          <Card body-class="p-4 space-y-3">
            <template #header>
              <Plus class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Assign a VPS</span>
            </template>

            <div v-if="serversStore.items.length === 0" class="text-[11px] text-muted-foreground">
              No VPS defined yet.
              <RouterLink to="/servers" class="text-primary hover:underline">Add one</RouterLink>
              to assign it here.
            </div>
            <template v-else>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">VPS</label>
                <AppSelect
                  size="sm"
                  :model-value="newDeployServerId"
                  :options="availableServerOptions"
                  @update:model-value="newDeployServerId = $event"
                />
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Role</label>
                <AppSelect
                  size="sm"
                  :model-value="newDeployRole"
                  :options="deployRoleOptions"
                  @update:model-value="newDeployRole = $event"
                />
                <Input
                  v-model="newDeployRole"
                  type="text"
                  size="sm"
                  placeholder="Or type a custom role (empty → production)"
                />
              </div>
              <Button
                :disabled="addingDeploy || !newDeployServerId"
                @click="handleAddDeployServer"
              >
                <Plus class="h-3.5 w-3.5" :stroke-width="2" />
                {{ addingDeploy ? 'Adding…' : 'Add' }}
              </Button>
            </template>
          </Card>
        </div>

        <!-- GitHub tab -->
        <div v-if="activeTab === 'github'" class="max-w-lg space-y-4">
          <Card body-class="p-4 space-y-4">
            <template #header>
              <Github class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Linked GitHub Account</span>
            </template>
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              Choose which GitHub account is used to fetch issues and pull requests for this project.
              Manage accounts in
              <RouterLink to="/settings" class="text-primary hover:underline">Settings</RouterLink>.
            </p>

            <div v-if="ghStore.accounts.length === 0" class="text-[11px] text-muted-foreground">
              No GitHub accounts yet.
              <RouterLink to="/settings" class="text-primary hover:underline">Add one in Settings</RouterLink>
              to link it here.
            </div>
            <AppSelect
              size="sm"
              v-else
              :model-value="linkedAccountId ?? ''"
              :options="accountOptions"
              placeholder="Select an account…"
              @update:model-value="handleSelectAccount"
            />

            <div v-if="linkedAccount" class="text-[11px] text-muted-foreground">
              <span v-if="linkedAccount.username">@{{ linkedAccount.username }}</span>
              <span v-if="linkedAccount.scopes.length"> · {{ linkedAccount.scopes.join(', ') }}</span>
            </div>
          </Card>

          <!-- Validate section -->
          <Card v-if="linkedAccountId" body-class="p-4 space-y-3">
            <template #header>
              <CheckCircle2 class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Validate Linked Account</span>
            </template>
            <Button
              variant="outline"
              :disabled="validating"
              @click="handleValidateAccount"
            >
              {{ validating ? 'Validating…' : 'Validate' }}
            </Button>
            <div
              v-if="accountValidation"
              class="p-3 bg-emerald-500/10 border border-emerald-500/20 rounded-md text-xs"
            >
              <div class="flex items-center gap-1.5 text-emerald-500 font-medium mb-1">
                <CheckCircle2 class="h-3.5 w-3.5" :stroke-width="2" />
                Valid — {{ accountValidation.username }}
              </div>
              <p class="text-muted-foreground">Scopes: {{ accountValidation.scopes.join(', ') || 'none' }}</p>
              <p v-if="!accountValidation.has_repo_scope" class="text-amber-500 mt-1 flex items-center gap-1">
                <AlertTriangle class="h-3 w-3" :stroke-width="1.75" />
                Missing repo/public_repo scope
              </p>
            </div>
            <div
              v-if="validationError"
              class="p-3 bg-destructive/10 border border-destructive/20 rounded-md text-xs"
            >
              <div class="flex items-center gap-1.5 text-destructive">
                <XCircle class="h-3.5 w-3.5" :stroke-width="1.75" />
                {{ validationError }}
              </div>
            </div>
          </Card>
        </div>

        <!-- GitLab tab -->
        <div v-if="activeTab === 'gitlab'" class="max-w-lg space-y-4">
          <Card body-class="p-4 space-y-4">
            <template #header>
              <Gitlab class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Linked GitLab Account</span>
            </template>
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              Choose which GitLab account is used to fetch issues and merge requests for this project.
              Manage accounts in
              <RouterLink to="/settings" class="text-primary hover:underline">Settings</RouterLink>.
            </p>

            <div v-if="glStore.accounts.length === 0" class="text-[11px] text-muted-foreground">
              No GitLab accounts yet.
              <RouterLink to="/settings" class="text-primary hover:underline">Add one in Settings</RouterLink>
              to link it here.
            </div>
            <AppSelect
              size="sm"
              v-else
              :model-value="linkedGitlabAccountId ?? ''"
              :options="gitlabAccountOptions"
              placeholder="Select an account…"
              @update:model-value="handleSelectGitlabAccount"
            />

            <div v-if="linkedGitlabAccount" class="text-[11px] text-muted-foreground">
              <span v-if="linkedGitlabAccount.username">@{{ linkedGitlabAccount.username }}</span>
              <span v-if="linkedGitlabAccount.host"> · {{ linkedGitlabAccount.host }}</span>
            </div>
          </Card>

          <!-- Validate section -->
          <Card v-if="linkedGitlabAccountId" body-class="p-4 space-y-3">
            <template #header>
              <CheckCircle2 class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
              <span class="text-xs font-semibold">Validate Linked Account</span>
            </template>
            <Button
              variant="outline"
              :disabled="validatingGitlab"
              @click="handleValidateGitlabAccount"
            >
              {{ validatingGitlab ? 'Validating…' : 'Validate' }}
            </Button>
            <div
              v-if="gitlabValidation"
              class="p-3 bg-emerald-500/10 border border-emerald-500/20 rounded-md text-xs"
            >
              <div class="flex items-center gap-1.5 text-emerald-500 font-medium mb-1">
                <CheckCircle2 class="h-3.5 w-3.5" :stroke-width="2" />
                Valid — {{ gitlabValidation.username }}
              </div>
              <p v-if="gitlabValidation.email" class="text-muted-foreground">{{ gitlabValidation.email }}</p>
            </div>
            <div
              v-if="gitlabValidationError"
              class="p-3 bg-destructive/10 border border-destructive/20 rounded-md text-xs"
            >
              <div class="flex items-center gap-1.5 text-destructive">
                <XCircle class="h-3.5 w-3.5" :stroke-width="1.75" />
                {{ gitlabValidationError }}
              </div>
            </div>
          </Card>
        </div>

        <!-- Conflicts tab -->
        <div v-if="activeTab === 'conflicts'" class="max-w-lg space-y-3">
          <!-- Skill conflicts -->
          <Card
            v-for="conflict in projectConflicts"
            :key="conflict.id"
            class="border-amber-500/20"
            bodyClass="p-4"
          >
            <div class="flex items-start gap-3 mb-3">
              <GitMerge class="h-4 w-4 text-amber-500 mt-0.5 shrink-0" :stroke-width="1.75" />
              <div>
                <div class="flex items-center gap-1.5">
                  <p class="text-sm font-mono font-medium">{{ conflict.skill_name }}</p>
                  <Badge tone="neutral" size="xs" class="uppercase tracking-wide">
                    Skill · {{ conflict.engine === 'codex' ? '.codex/skills' : '.claude/skills' }}
                  </Badge>
                </div>
                <p class="text-xs text-muted-foreground mt-0.5">
                  Local changes detected · {{ new Date(conflict.detected_at).toLocaleDateString() }}
                </p>
              </div>
            </div>
            <div class="flex gap-2">
              <Button
                class="flex-1"
                @click="projectStore.resolveConflict(conflict.id, true); projectStore.fetchConflicts()"
              >
                Overwrite with central
              </Button>
              <Button
                variant="outline"
                class="flex-1"
                @click="projectStore.resolveConflict(conflict.id, false); projectStore.fetchConflicts()"
              >
                Keep local
              </Button>
            </div>
          </Card>

          <!-- Rule conflicts -->
          <Card
            v-for="conflict in projectRuleConflicts"
            :key="conflict.id"
            class="border-amber-500/20"
            bodyClass="p-4"
          >
            <div class="flex items-start gap-3 mb-3">
              <GitMerge class="h-4 w-4 text-amber-500 mt-0.5 shrink-0" :stroke-width="1.75" />
              <div>
                <div class="flex items-center gap-1.5">
                  <p class="text-sm font-mono font-medium">{{ conflict.rule_name }}</p>
                  <Badge tone="neutral" size="xs" class="uppercase tracking-wide">
                    Rule · {{ conflict.engine === 'claude' ? '.claude/rules' : 'AGENTS.md' }}
                  </Badge>
                </div>
                <p class="text-xs text-muted-foreground mt-0.5">
                  Local changes detected · {{ new Date(conflict.detected_at).toLocaleDateString() }}
                </p>
              </div>
            </div>
            <div class="flex gap-2">
              <Button
                class="flex-1"
                @click="projectStore.resolveRuleConflict(conflict.id, true)"
              >
                Overwrite with central
              </Button>
              <Button
                variant="outline"
                class="flex-1"
                @click="projectStore.resolveRuleConflict(conflict.id, false)"
              >
                Keep local
              </Button>
            </div>
          </Card>
        </div>

        </div>
      </div>
    </div>
  </div>
</template>
