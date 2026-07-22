<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useProjectsStore, type DetectedProjectInfo, type DetectedRepo } from '@/stores/projects'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { useGitlabAccountsStore } from '@/stores/gitlabAccounts'
import { useServersStore, type ProjectServer } from '@/stores/servers'
import { useAwsAccountsStore } from '@/stores/awsAccounts'
import { open } from '@tauri-apps/plugin-dialog'
import { invoke } from '@/lib/tauri'
import { Plus, FolderOpen, GitBranch, Github, Gitlab, Trash2, X, SquareTerminal, Code2, Settings, HardDrive, Cloud } from 'lucide-vue-next'
import { Button, Input, Badge, Modal, Card } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'

const router = useRouter()
const store = useProjectsStore()
const ghStore = useGithubAccountsStore()
const glStore = useGitlabAccountsStore()
const serversStore = useServersStore()
const awsStore = useAwsAccountsStore()
const { confirm } = useConfirm()
const adding = ref(false)
const detecting = ref(false)

function accountLabel(accountId: string | null): string | null {
  if (!accountId) return null
  return ghStore.accounts.find(a => a.id === accountId)?.label ?? 'Unknown'
}

function gitlabAccountLabel(accountId: string | null): string | null {
  if (!accountId) return null
  return glStore.accounts.find(a => a.id === accountId)?.label ?? 'Unknown'
}

// The AWS account linked to a project (label + region shown on its chip).
function awsAccount(accountId: string | null) {
  if (!accountId) return null
  return awsStore.accounts.find(a => a.id === accountId) ?? null
}

// Mapped VPS servers per project (project_servers has no bulk endpoint, so we
// fan out list_project_servers once after the projects load).
const MAX_SERVER_CHIPS = 2
type SrvTone = 'success' | 'error' | 'neutral'
const projectServers = ref<Record<string, ProjectServer[]>>({})
async function loadProjectServers() {
  const entries = await Promise.all(
    store.projects.map(async (p): Promise<[string, ProjectServer[]]> => {
      try {
        return [p.id, await serversStore.listForProject(p.id)]
      } catch {
        return [p.id, []]
      }
    }),
  )
  projectServers.value = Object.fromEntries(entries)
}

// Decorate each project's servers into header-style chips (tone by status,
// collapsing the overflow into a single "+N").
const projectChips = computed(() => {
  const map: Record<string, {
    visible: { key: string; label: string; tone: SrvTone; title: string }[]
    hiddenCount: number
    hiddenTitle: string
  }> = {}
  for (const p of store.projects) {
    const chips = (projectServers.value[p.id] ?? []).map((s) => {
      const tone: SrvTone = s.status === 'online' ? 'success' : s.status === 'offline' ? 'error' : 'neutral'
      const status = s.status ? ` · ${s.status}` : ''
      return {
        key: `${s.id}-${s.role}`,
        label: s.label,
        tone,
        title: `${s.role}: ${s.username}@${s.host}:${s.port}${status}`,
      }
    })
    map[p.id] = {
      visible: chips.slice(0, MAX_SERVER_CHIPS),
      hiddenCount: Math.max(0, chips.length - MAX_SERVER_CHIPS),
      hiddenTitle: chips.slice(MAX_SERVER_CHIPS).map((c) => c.label).join('\n'),
    }
  }
  return map
})

interface PendingRepo extends DetectedRepo {
  _key: number
}

let _keyCounter = 0

const pending = ref<DetectedProjectInfo | null>(null)
const pendingName = ref('')
const pendingRepos = ref<PendingRepo[]>([])

onMounted(async () => {
  await store.fetchProjects()
  if (ghStore.accounts.length === 0) ghStore.fetch()
  if (glStore.accounts.length === 0) glStore.fetch()
  if (awsStore.accounts.length === 0) awsStore.fetch()
  loadProjectServers()
})

async function handleAdd() {
  const selected = await open({
    directory: true,
    multiple: false,
    title: 'Select Project Folder',
  })
  if (!selected) return
  detecting.value = true
  try {
    const info = await store.detectProjectInfo(selected)
    pending.value = info
    pendingName.value = info.name
    pendingRepos.value = info.repos.map(r => ({ ...r, _key: _keyCounter++ }))
    if (pendingRepos.value.length === 0) {
      pendingRepos.value.push({ name: '', path: selected, github_owner: null, github_repo: null, _key: _keyCounter++ })
    }
  } catch (e) {
    alert(String(e))
  } finally {
    detecting.value = false
  }
}

function addRepoRow() {
  pendingRepos.value.push({
    name: '',
    path: pending.value?.path ?? '',
    github_owner: null,
    github_repo: null,
    _key: _keyCounter++,
  })
}

function removeRepoRow(key: number) {
  pendingRepos.value = pendingRepos.value.filter(r => r._key !== key)
}

function cancelAdd() {
  pending.value = null
  pendingRepos.value = []
}

async function confirmAdd() {
  if (!pending.value) return
  adding.value = true
  try {
    const repos = pendingRepos.value
      .filter(r => r.name.trim())
      .map(r => ({
        name: r.name.trim(),
        path: r.path,
        github_owner: r.github_owner || undefined,
        github_repo: r.github_repo || undefined,
      }))

    const project = await store.addProject({
      path: pending.value.path,
      name: pendingName.value || undefined,
      repos: repos.length > 0 ? repos : undefined,
    })
    pending.value = null
    pendingRepos.value = []
    router.push(`/projects/${project.id}`)
  } catch (e) {
    alert(String(e))
  } finally {
    adding.value = false
  }
}

async function handleRemove(project: { id: string; name: string }) {
  if (!(await confirm({
    title: 'Remove project',
    message: `Remove project "${project.name}"? (Files are NOT deleted)`,
    confirmLabel: 'Remove',
  }))) return
  try {
    await store.removeProject(project.id)
  } catch (e) {
    alert(String(e))
  }
}

async function handleOpenInTerminal(project: { path: string }) {
  try {
    const settings = await invoke<{ terminal_app: string }>('get_settings')
    await store.openInTerminal(project.path, settings.terminal_app)
  } catch (e) {
    alert(String(e))
  }
}

async function handleOpenInVscode(project: { path: string }) {
  try {
    await store.openInVscode(project.path)
  } catch (e) {
    alert(String(e))
  }
}

async function handleOpenInFolder(project: { path: string }) {
  try {
    await store.openInFolder(project.path)
  } catch (e) {
    alert(String(e))
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">Projects</h1>
        <span
          v-if="!store.loading && store.projects.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.projects.length }}
        </span>
      </div>
      <Button
        :disabled="adding || detecting"
        @click="handleAdd"
      >
        <Plus class="h-3.5 w-3.5" :stroke-width="2" />
        {{ detecting ? 'Detecting…' : adding ? 'Adding…' : 'Add Project' }}
      </Button>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <div v-if="store.loading" class="grid grid-cols-1 gap-3 sm:grid-cols-2 2xl:grid-cols-3">
        <div v-for="i in 8" :key="i" class="h-32 rounded-xl border border-border bg-card animate-pulse" />
      </div>

      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <div v-else-if="store.projects.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <FolderOpen class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">No projects yet</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-50">Add a local folder to start managing AI skills and running analyses</p>
        <Button
          class="mt-4"
          @click="handleAdd"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          Add Project
        </Button>
      </div>

      <div v-else class="grid grid-cols-1 sm:grid-cols-2 2xl:grid-cols-3 gap-3">
        <Card
          v-for="project in store.projects"
          :key="project.id"
          class="group relative flex flex-col transition-all duration-150 cursor-pointer hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)] hover:-translate-y-0.5"
          body-class="flex flex-col flex-1 p-4"
          @click="router.push(`/projects/${project.id}`)"
        >
          <!-- hover accent line -->
          <span class="absolute inset-x-0 top-0 h-0.5 origin-left scale-x-0 bg-linear-to-r from-primary to-primary/30 transition-transform duration-200 group-hover:scale-x-100" />

          <!-- Icon + name/path -->
          <div class="flex items-start gap-3 mb-3">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary transition-colors group-hover:bg-primary/15 group-hover:border-primary/25">
              <FolderOpen class="h-4.5 w-4.5" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5">
                <p class="text-sm font-semibold truncate leading-tight">{{ project.name }}</p>
              </div>
              <p class="text-xs text-muted-foreground mt-1 line-clamp-2 leading-relaxed font-mono break-all">{{ project.path }}</p>
            </div>
          </div>

          <!-- Footer -->
          <div class="flex items-start justify-between gap-2 pt-2.5 border-t border-border/60 mt-auto">
            <div class="flex items-center gap-1.5 flex-wrap min-w-0">
              <Badge v-if="project.github_account_id" tone="info" class="max-w-[9rem]">
                <Github class="h-2.5 w-2.5 shrink-0" :stroke-width="2" />
                <span class="truncate">{{ accountLabel(project.github_account_id) }}</span>
              </Badge>
              <Badge v-if="project.gitlab_account_id" tone="warning" class="max-w-[9rem]">
                <Gitlab class="h-2.5 w-2.5 shrink-0" :stroke-width="2" />
                <span class="truncate">{{ gitlabAccountLabel(project.gitlab_account_id) }}</span>
              </Badge>
              <!-- Mapped VPS servers (tone by connection status). -->
              <Badge
                v-for="chip in projectChips[project.id]?.visible"
                :key="chip.key"
                :tone="chip.tone"
                :title="chip.title"
                class="max-w-[9rem]"
              >
                <HardDrive class="h-2.5 w-2.5 shrink-0" :stroke-width="2" />
                <span class="truncate">{{ chip.label }}</span>
              </Badge>
              <Badge
                v-if="projectChips[project.id]?.hiddenCount"
                tone="neutral"
                :title="projectChips[project.id]?.hiddenTitle"
              >
                <HardDrive class="h-2.5 w-2.5" :stroke-width="2" />
                +{{ projectChips[project.id].hiddenCount }}
              </Badge>
              <!-- Linked AWS account. -->
              <Badge
                v-if="awsAccount(project.aws_account_id)"
                tone="primary"
                class="max-w-[9rem]"
                :title="`AWS: ${awsAccount(project.aws_account_id)!.label} · region ${awsAccount(project.aws_account_id)!.region}`"
              >
                <Cloud class="h-2.5 w-2.5 shrink-0" :stroke-width="2" />
                <span class="truncate">{{ awsAccount(project.aws_account_id)!.label }}</span>
              </Badge>
            </div>
            <!-- Actions on hover -->
            <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity" @click.stop>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Open project in VS Code"
                @click="handleOpenInVscode(project)"
              >
                <Code2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Open project folder"
                @click="handleOpenInFolder(project)"
              >
                <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Open project folder in terminal"
                @click="handleOpenInTerminal(project)"
              >
                <SquareTerminal class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Project settings"
                @click="router.push(`/projects/${project.id}/settings`)"
              >
                <Settings class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-destructive/60 hover:text-destructive hover:bg-destructive/10 transition-colors cursor-pointer"
                title="Remove project"
                @click="handleRemove(project)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
            </div>
          </div>
        </Card>
      </div>
    </div>

    <!-- Add project modal -->
    <Modal :open="!!pending" size="md" scroll-body title="Add Project" @close="cancelAdd">
      <template v-if="pending">
          <div class="px-5 py-5 flex flex-col gap-4">
            <!-- Path (read-only) -->
            <div>
              <label class="block text-xs text-muted-foreground mb-1.5">Path</label>
              <div class="flex items-center gap-2 px-3 py-2 bg-muted/50 border border-border/60 rounded-md">
                <FolderOpen class="h-3.5 w-3.5 text-muted-foreground shrink-0" :stroke-width="1.5" />
                <span class="text-xs font-mono text-muted-foreground truncate">{{ pending.path }}</span>
              </div>
            </div>

            <!-- Name -->
            <div>
              <label class="block text-xs text-muted-foreground mb-1.5">Project name</label>
              <Input
                v-model="pendingName"
                placeholder="My Project"
              />
            </div>

            <!-- Repos -->
            <div>
              <div class="flex items-center justify-between mb-2">
                <label class="flex items-center gap-1.5 text-xs text-muted-foreground">
                  <GitBranch class="h-3 w-3" :stroke-width="1.5" />
                  Repositories
                  <span v-if="pending.repos.length > 0" class="text-[10px] text-emerald-500 font-medium">
                    {{ pending.repos.length }} auto-detected
                  </span>
                </label>
                <button
                  class="flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground transition-colors cursor-pointer"
                  @click="addRepoRow"
                >
                  <Plus class="h-3 w-3" :stroke-width="2" />
                  Add
                </button>
              </div>

              <div class="space-y-2">
                <div
                  v-for="repo in pendingRepos"
                  :key="repo._key"
                  class="flex flex-col gap-1.5 p-3 bg-muted/30 border border-border/60 rounded-md"
                >
                  <!-- Repo path (read-only) -->
                  <div class="flex items-center gap-1.5 text-[10px] text-muted-foreground font-mono truncate">
                    <FolderOpen class="h-2.5 w-2.5 shrink-0" :stroke-width="1.5" />
                    {{ repo.path }}
                  </div>
                  <!-- Name + delete -->
                  <div class="flex gap-2 items-center">
                    <Input
                      v-model="repo.name"
                      class="flex-1"
                      placeholder="Repo name"
                    />
                    <button
                      class="flex h-6 w-6 items-center justify-center rounded text-destructive/40 hover:text-destructive hover:bg-destructive/10 transition-colors cursor-pointer shrink-0"
                      @click="removeRepoRow(repo._key)"
                    >
                      <X class="h-3 w-3" :stroke-width="2" />
                    </button>
                  </div>
                  <!-- Owner / Repo -->
                  <div class="flex gap-1.5 items-center">
                    <Input
                      v-model="repo.github_owner"
                      placeholder="owner"
                    />
                    <span class="text-muted-foreground text-xs shrink-0">/</span>
                    <Input
                      v-model="repo.github_repo"
                      placeholder="repo"
                    />
                  </div>
                </div>

                <div v-if="pendingRepos.length === 0" class="text-[11px] text-muted-foreground text-center py-2">
                  No repositories — click Add to add one manually
                </div>
              </div>
            </div>
          </div>

      </template>

      <template #footer>
        <Button variant="ghost" @click="cancelAdd">
          Cancel
        </Button>
        <Button
          :disabled="adding || !pendingName.trim()"
          @click="confirmAdd"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ adding ? 'Adding…' : 'Add Project' }}
        </Button>
      </template>
    </Modal>
  </div>
</template>
