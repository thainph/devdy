<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { invoke } from '@tauri-apps/api/core'
import { useMcpServersStore, type McpServer } from '@/stores/mcpServers'
import { useAppSettingsStore } from '@/stores/appSettings'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Button, Card, Badge } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { Plus, Upload, Download, Pencil, Trash2, Server, CalendarDays, Power, AlertTriangle, Sparkles, Settings2 } from 'lucide-vue-next'

const router = useRouter()
const store = useMcpServersStore()
const appSettings = useAppSettingsStore()
const { confirm } = useConfirm()
const { toast } = useToast()
const { t } = useI18n()
const deletingId = ref<string | null>(null)
const togglingId = ref<string | null>(null)
const importing = ref(false)
const addingName = ref<string | null>(null)

// The built-in `devdy` MCP server is injected at run launch (not a row in the
// list); it's toggled from Settings → MCP Server.
const builtinEnabled = computed(() => appSettings.settings?.mcp_builtin_devdy_enabled !== 'false')

// Google-backed built-ins (gdrive/gmail) are injected at run launch only when an
// OAuth client is configured AND at least one account is connected (mirrors the
// backend `with_builtin_google` gate). Fetch that status so the cards below can
// reflect it accurately instead of pretending only `devdy` exists.
const googleAccountCount = ref(0)
const googleHasClient = ref(false)
async function loadGoogleStatus() {
  try {
    const accounts = await invoke<unknown[]>('list_google_accounts')
    googleAccountCount.value = Array.isArray(accounts) ? accounts.length : 0
    googleHasClient.value = (await invoke<{ has_client: boolean }>('google_client_status')).has_client
  } catch {
    googleAccountCount.value = 0
    googleHasClient.value = false
  }
}

// The 3 native Devdy MCP servers, each with its real runtime status so the
// screen shows the truth (active / disabled / needs setup) rather than a single
// hardcoded devdy card.
type NativeStatus = { tone: 'success' | 'neutral' | 'warning'; label: string }
interface NativeServer {
  name: string
  tools: string
  description: string
  status: NativeStatus
  section: string
}
const nativeServers = computed<NativeServer[]>(() => {
  const google: NativeStatus = !googleHasClient.value
    ? { tone: 'warning', label: 'Built-in · needs OAuth client' }
    : googleAccountCount.value === 0
      ? { tone: 'warning', label: 'Built-in · no account' }
      : { tone: 'success', label: 'Built-in · active' }
  return [
    {
      name: 'devdy',
      tools: 'mcp__devdy__*',
      description:
        'Auto-injected into every run. Gives the AI your notes, cross-session recall, project context (file tree & git) and managed VPS.',
      status: builtinEnabled.value
        ? { tone: 'success', label: 'Built-in · active' }
        : { tone: 'neutral', label: 'Built-in · disabled' },
      section: 'mcp',
    },
    {
      name: 'gdrive',
      tools: 'mcp__gdrive__*',
      description:
        'Google Drive: list, search, read, upload, update, share and delete files. Injected when a Google account is connected.',
      status: google,
      section: 'google',
    },
    {
      name: 'gmail',
      tools: 'mcp__gmail__*',
      description:
        'Gmail: list, search, read, send, reply, draft and manage labels. Injected when a Google account is connected.',
      status: google,
      section: 'google',
    },
  ]
})

// One-click catalog of commonly useful third-party MCP servers. Adding one
// creates a disabled-secrets stub the user finishes in the editor (args/paths/
// tokens). Names must be unique, so already-added entries are filtered out.
interface CatalogEntry {
  name: string
  description: string
  transport: McpServer['transport']
  command?: string
  args?: string[]
  url?: string
  env?: string[]
}
const CATALOG: CatalogEntry[] = [
  { name: 'filesystem', description: 'Read/write files in an allow-listed directory.', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', '<ALLOWED_DIR>'] },
  { name: 'sequential-thinking', description: 'Structured step-by-step reasoning scratchpad.', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-sequential-thinking'] },
  { name: 'memory', description: 'Persistent knowledge-graph memory across turns.', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-memory'] },
  { name: 'github', description: 'Query & manage GitHub repos, issues and PRs.', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'], env: ['GITHUB_PERSONAL_ACCESS_TOKEN'] },
  { name: 'context7', description: 'Up-to-date library docs to reduce hallucinated APIs.', transport: 'http', url: 'https://mcp.context7.com/mcp' },
  { name: 'playwright', description: 'Drive a real browser for testing & scraping.', transport: 'stdio', command: 'npx', args: ['-y', '@playwright/mcp@latest'] },
]
const catalogAvailable = computed(() => CATALOG.filter(c => !store.items.some(s => s.name === c.name)))

async function addFromCatalog(item: CatalogEntry) {
  addingName.value = item.name
  try {
    await store.createServer({
      name: item.name,
      description: item.description,
      transport: item.transport,
      command: item.command ?? null,
      args: item.args ?? [],
      url: item.url ?? null,
      env: (item.env ?? []).map(key => ({ key, value: '' })),
      headers: [],
      enabled: true,
    })
    toast.success(t('mcp.list.toast.added', { name: item.name }))
  } catch (e) {
    toast.error(String(e))
  } finally {
    addingName.value = null
  }
}

// The MCP list is global (not project-scoped). Codex supports stdio and
// streamable HTTP; legacy SSE remains Claude-only.
const defaultIsCodex = computed(() => appSettings.settings?.default_engine === 'codex')

onMounted(() => {
  store.fetchServers()
  appSettings.ensureLoaded()
  loadGoogleStatus()
})

function isSse(server: McpServer): boolean {
  return server.transport === 'sse'
}

async function handleImport() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'MCP Server', extensions: ['json'] }],
  })
  if (!selected) return
  importing.value = true
  try {
    await store.importServer(selected as string)
    toast.success(t('mcp.list.toast.imported'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    importing.value = false
  }
}

async function handleExport(server: McpServer) {
  const destPath = await save({
    defaultPath: `${server.name}.json`,
    filters: [{ name: 'JSON', extensions: ['json'] }],
  })
  if (!destPath) return
  try {
    await store.exportServer(server.id, destPath)
    toast.success(t('mcp.list.toast.exported'))
  } catch (e) {
    toast.error(String(e))
  }
}

async function handleDelete(server: McpServer) {
  if (!(await confirm({
    title: 'Delete MCP server',
    message: `Delete MCP server "${server.name}"? This also removes its stored secrets. This cannot be undone.`,
    confirmLabel: 'Delete',
  }))) return
  deletingId.value = server.id
  try {
    await store.deleteServer(server.id)
    toast.success('Deleted')
  } catch (e) {
    toast.error(String(e))
  } finally {
    deletingId.value = null
  }
}

// Toggling `enabled` re-uses update. Secret rows are sent as key-only entries
// (value omitted) so the backend keeps the stored VALUEs untouched.
async function handleToggleEnabled(server: McpServer) {
  togglingId.value = server.id
  try {
    await store.updateServer({
      id: server.id,
      name: server.name,
      description: server.description,
      transport: server.transport,
      command: server.command,
      args: server.args,
      url: server.url,
      env: server.env_keys.map(key => ({ key })),
      headers: server.header_keys.map(key => ({ key })),
      enabled: !server.enabled,
    })
    toast.success(server.enabled ? 'Disabled' : 'Enabled')
  } catch (e) {
    toast.error(String(e))
  } finally {
    togglingId.value = null
  }
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">MCP Servers</h1>
        <span
          v-if="!store.loading && store.items.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.items.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button
          variant="outline"
          :disabled="importing"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ importing ? 'Importing…' : 'Import' }}
        </Button>
        <Button @click="router.push('/mcp/new')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New Server
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6 space-y-6">
      <!-- Built-in native servers (injected at run launch, not list rows) -->
      <div class="space-y-2">
        <div class="flex items-center gap-2">
          <Sparkles class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
          <h2 class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Built-in servers</h2>
        </div>
        <div
          v-for="native in nativeServers"
          :key="native.name"
          class="rounded-lg border p-4 flex items-start gap-3"
          :class="native.status.tone === 'success' ? 'border-primary/30 bg-primary/5' : 'border-border/60 bg-muted/20 opacity-70'"
        >
          <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary">
            <Server class="h-4.5 w-4.5" :stroke-width="1.75" />
          </div>
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2 flex-wrap">
              <p class="text-sm font-semibold font-mono leading-tight">{{ native.name }}</p>
              <Badge :tone="native.status.tone" size="xs" class="shrink-0">
                {{ native.status.label }}
              </Badge>
            </div>
            <p class="text-xs text-muted-foreground mt-1 leading-relaxed">
              {{ native.description }} Exposed as <code>{{ native.tools }}</code> tools.
            </p>
          </div>
          <Button variant="outline" size="sm" class="shrink-0" @click="router.push({ path: '/settings', query: { section: native.section } })">
            <Settings2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            Settings
          </Button>
        </div>
      </div>

      <!-- Loading skeleton -->
      <div v-if="store.loading" class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <div v-for="i in 5" :key="i" class="h-27 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="store.items.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <Server class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">No MCP servers yet</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-50">Define an MCP server once, then enable it per project</p>
        <Button class="mt-4" @click="router.push('/mcp/new')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New Server
        </Button>
      </div>

      <!-- Cards grid -->
      <div v-else class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <Card
          v-for="server in store.items"
          :key="server.id"
          class="group relative flex flex-col transition-all duration-150 cursor-pointer hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)] hover:-translate-y-0.5"
          body-class="flex flex-col flex-1 p-4"
          :class="!server.enabled && 'opacity-60'"
          @click="router.push(`/mcp/${server.id}/edit`)"
        >
          <!-- hover accent line -->
          <span class="absolute inset-x-0 top-0 h-0.5 origin-left scale-x-0 bg-linear-to-r from-primary to-primary/30 transition-transform duration-200 group-hover:scale-x-100" />

          <!-- Icon + name/desc -->
          <div class="flex items-start gap-3 mb-3">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary transition-colors group-hover:bg-primary/15 group-hover:border-primary/25">
              <Server class="h-4.5 w-4.5" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5 flex-wrap">
                <p class="text-sm font-semibold font-mono truncate leading-tight">{{ server.name }}</p>
                <Badge
                  v-if="isSse(server) && defaultIsCodex"
                  tone="warning"
                  size="xs"
                  class="shrink-0"
                  title="SSE transport is only supported by Claude. Codex runs will skip this server."
                >
                  <AlertTriangle class="h-2.5 w-2.5" :stroke-width="2" />
                  Claude only
                </Badge>
              </div>
              <p class="text-xs text-muted-foreground mt-1 line-clamp-2 leading-relaxed">{{ server.description }}</p>
            </div>
          </div>

          <!-- Footer -->
          <div class="flex items-center justify-between pt-2.5 border-t border-border/60 mt-auto">
            <div class="flex items-center gap-2 min-w-0">
              <Badge tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">
                {{ server.transport }}
              </Badge>
              <div class="flex items-center gap-1 text-[10px] text-muted-foreground/60">
                <CalendarDays class="h-3 w-3" :stroke-width="1.5" />
                <span>{{ formatDate(server.created_at) }}</span>
              </div>
            </div>
            <!-- Actions on hover -->
            <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity" @click.stop>
              <button
                class="flex h-6 w-6 items-center justify-center rounded transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-default"
                :class="server.enabled ? 'text-emerald-500 hover:bg-accent' : 'text-muted-foreground hover:text-foreground hover:bg-accent'"
                :title="server.enabled ? 'Disable' : 'Enable'"
                :disabled="togglingId === server.id"
                @click="handleToggleEnabled(server)"
              >
                <Power class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Export JSON (includes secrets)"
                @click="handleExport(server)"
              >
                <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Edit"
                @click="router.push(`/mcp/${server.id}/edit`)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-destructive/60 hover:text-destructive hover:bg-destructive/10 transition-colors cursor-pointer disabled:opacity-40"
                title="Delete"
                :disabled="deletingId === server.id"
                @click="handleDelete(server)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
            </div>
          </div>
        </Card>
      </div>

      <!-- Recommended catalog (one-click add) -->
      <div v-if="!store.loading && catalogAvailable.length" class="space-y-3">
        <div class="flex items-center gap-2">
          <Sparkles class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
          <h2 class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Recommended servers</h2>
        </div>
        <div class="grid grid-cols-2 xl:grid-cols-3 gap-3">
          <Card
            v-for="item in catalogAvailable"
            :key="item.name"
            class="flex flex-col"
            body-class="flex flex-col flex-1 p-4"
          >
            <div class="flex items-start justify-between gap-2 mb-1">
              <p class="text-sm font-semibold font-mono leading-tight truncate">{{ item.name }}</p>
              <Badge tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">{{ item.transport }}</Badge>
            </div>
            <p class="text-xs text-muted-foreground line-clamp-2 leading-relaxed flex-1">{{ item.description }}</p>
            <Button
              variant="outline"
              size="sm"
              class="mt-3 self-start"
              :disabled="addingName === item.name"
              @click="addFromCatalog(item)"
            >
              <Plus class="h-3.5 w-3.5" :stroke-width="2" />
              {{ addingName === item.name ? 'Adding…' : 'Add' }}
            </Button>
          </Card>
        </div>
      </div>
    </div>
  </div>
</template>
