<script setup lang="ts">
// MCP servers, Notes-style master-detail:
//
//   ┌ sidebar: search · import · refresh   │ detail: name / transport · enabled │
//   │ + New, All | Enabled | Disabled      │ · added   [switch][Test][Save][⋯]  │
//   │ Built-in · stdio · http · sse ·      │ General / Process+Environment or   │
//   │ Recommended folders                  │ Endpoint+Headers                   │
//   └──────────────────────────────────────┴────────────────────────────────────┘
//
// Folders are by transport: it decides the whole shape of a server's config
// (process + env vs. URL + headers) and Codex support (sse is Claude-only);
// the type dropdown narrows to one folder. The built-in native
// servers and the not-yet-added catalog are rows too, with a read-only pane.
//
// Selection lives in the URL (`?id=` a server or a `builtin:` / `catalog:`
// row, `?new=1` a blank draft) — never anything secret.
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Plus, RotateCw, Server, Upload } from 'lucide-vue-next'
import { AppSelect, Button } from '@/components/ui'
import ListDetailSidebar from '@/components/listDetail/ListDetailSidebar.vue'
import DetailEmpty from '@/components/listDetail/DetailEmpty.vue'
import McpServerDetail from '@/components/mcp/McpServerDetail.vue'
import McpInfoDetail from '@/components/mcp/McpInfoDetail.vue'
import { useActivatedRefresh } from '@/composables/useActivatedRefresh'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useMcpServersStore, type McpServer } from '@/stores/mcpServers'
import { useAppSettingsStore } from '@/stores/appSettings'
import {
  MCP_CATALOG,
  availableCatalog,
  builtinRow,
  catalogRow,
  copyName,
  duplicateForm,
  formFromServer,
  groupMcpRows,
  mcpRowType,
  MCP_TYPES,
  GROUP_CATALOG,
  matchesMcpRow,
  serverRow,
  type BuiltinInfo,
  type CatalogEntry,
  type McpForm,
  type McpRow,
} from '@/lib/mcpRows'

// Named explicitly: <KeepAlive :include> in App.vue matches on component name,
// and an inferred name is a build detail that minification can change.
defineOptions({ name: 'McpServersView' })

const route = useRoute()
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

// ── Built-in native servers ─────────────────────────────────────────────────

// The built-in `devdy` MCP server is injected at run launch (not a stored
// server); it's toggled from Settings → MCP Server.
const builtinEnabled = computed(() => appSettings.settings?.mcp_builtin_devdy_enabled !== 'false')

// The Google-backed built-in is injected at run launch only when an OAuth client
// is configured AND at least one account is connected (mirrors the backend
// `with_builtin_google` gate). Fetch that status so its row reflects it.
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

interface Builtin extends BuiltinInfo {
  description: string
  status: string
  /** Settings section that configures it. */
  section: string
}
const builtins = computed<Builtin[]>(() => {
  const devdy: Builtin = {
    name: 'devdy',
    tools: 'mcp__devdy__*',
    tone: builtinEnabled.value ? 'success' : 'neutral',
    status: builtinEnabled.value ? t('mcp.list.builtin.active') : t('mcp.list.builtin.disabled'),
    description: t('mcp.list.builtin.description', { tools: 'mcp__devdy__*' }),
    section: 'mcp',
  }
  const googleState = !googleHasClient.value ? 'needsClient' : googleAccountCount.value === 0 ? 'noAccount' : 'active'
  const google: Builtin = {
    name: 'google',
    tools: 'mcp__google__*',
    tone: googleState === 'active' ? 'success' : 'warning',
    status: t(`mcp.list.builtin.${googleState}`),
    description: t('mcp.list.builtin.googleDescription', { tools: 'mcp__google__*' }),
    section: 'google',
  }
  return [devdy, google]
})

// ── Sidebar rows ────────────────────────────────────────────────────────────

const search = ref('')
/** '' = all types, else a folder key (see mcpRowType). */
const typeFilter = ref('')

const typeOptions = computed(() => [
  { value: '', label: t('mcp.list.allTypes') },
  ...MCP_TYPES.map((type) => ({ value: type, label: typeLabel(type) })),
])

function typeLabel(type: string): string {
  if (type === GROUP_CATALOG) return t('mcp.list.recommended')
  return t(`mcp.list.groups.${type}`)
}

const catalog = computed(() => availableCatalog(MCP_CATALOG, store.items))

const allRows = computed<McpRow[]>(() => [
  ...builtins.value.map((b) => builtinRow(b, b.description)),
  ...store.items.map(serverRow),
  ...catalog.value.map(catalogRow),
])

const groups = computed(() =>
  groupMcpRows(
    allRows.value.filter((r) =>
      (!typeFilter.value || mcpRowType(r) === typeFilter.value)
      && matchesMcpRow(r, search.value)),
    {
      builtin: t('mcp.list.groups.builtin'),
      catalog: t('mcp.list.recommended'),
      transport: (tr) => t(`mcp.list.groups.${tr}`),
    },
  ),
)

// The MCP list is global (not project-scoped). Codex supports stdio and
// streamable HTTP; legacy SSE remains Claude-only.
const defaultIsCodex = computed(() => appSettings.settings?.default_engine === 'codex')

// ── Selection (URL query) ───────────────────────────────────────────────────

const selectedId = ref<string | null>(null)
const isNew = ref(false)
/** Bumped by "+ New" so a fresh draft remounts the pane. */
const draftKey = ref(0)
/** A draft's prefill (Duplicate); kept in memory, never in the URL. */
const draftSeed = ref<McpForm | null>(null)
const detailRef = ref<InstanceType<typeof McpServerDetail> | null>(null)

function queryToString(q: { id: string | null; isNew: boolean }): string {
  return q.isNew ? 'new' : q.id ?? ''
}

/** Puts the selection in the URL; the refs change first so the watcher below sees no news. */
function setSelection(id: string | null, draft = false) {
  selectedId.value = draft ? null : id
  isNew.value = draft
  if (!draft) draftSeed.value = null
  router.replace({ path: '/mcp', query: draft ? { new: '1' } : id ? { id } : {} }).catch(() => {})
}

function restoreQuery() {
  router
    .replace({ path: '/mcp', query: isNew.value ? { new: '1' } : selectedId.value ? { id: selectedId.value } : {} })
    .catch(() => {})
}

// KeepAlive keeps this view alive on other routes: only /mcp's own query
// drives the selection. A bare /mcp (the nav link) restores the current one; a
// different ?id= / ?new=1 from outside (e.g. an old editor URL) asks first
// when there are unsaved edits.
watch(
  () => [route.name, route.query.id, route.query.new],
  async () => {
    if (route.name !== 'mcp') return
    const id = typeof route.query.id === 'string' && route.query.id ? route.query.id : null
    const incoming = { id, isNew: !id && route.query.new === '1' }
    const current = { id: selectedId.value, isNew: isNew.value }
    if (!incoming.id && !incoming.isNew) {
      if (current.id || current.isNew) restoreQuery()
      return
    }
    if (queryToString(incoming) === queryToString(current)) return
    if (!(await confirmDiscard())) {
      restoreQuery()
      return
    }
    if (incoming.isNew) draftKey.value++
    selectedId.value = incoming.id
    isNew.value = incoming.isNew
    if (!incoming.isNew) draftSeed.value = null
  },
  { immediate: true },
)

const selectedServer = computed(() => store.items.find((s) => s.id === selectedId.value) ?? null)
const selectedRow = computed(() => allRows.value.find((r) => r.id === selectedId.value) ?? null)
const selectedEntry = computed(() =>
  selectedRow.value?.kind === 'catalog' ? MCP_CATALOG.find((c) => c.name === selectedRow.value?.name) ?? null : null,
)
const selectedBuiltin = computed(() =>
  selectedRow.value?.kind === 'builtin' ? builtins.value.find((b) => b.name === selectedRow.value?.name) ?? null : null,
)

/** Leaving unsaved edits takes a confirm; true = go ahead. */
async function confirmDiscard(): Promise<boolean> {
  if (!detailRef.value?.dirty) return true
  return confirm({
    title: t('mcp.detail.confirmDiscard.title'),
    message: t('mcp.detail.confirmDiscard.message'),
    confirmLabel: t('mcp.detail.confirmDiscard.confirm'),
  })
}

async function select(id: string) {
  if (id === selectedId.value && !isNew.value) return
  if (!(await confirmDiscard())) return
  setSelection(id)
}

/** Opens a blank (or prefilled) draft; false = the user kept their edits. */
async function startNew(seed: McpForm | null = null): Promise<boolean> {
  if (!(await confirmDiscard())) return false
  draftKey.value++
  setSelection(null, true)
  draftSeed.value = seed
  return true
}

// The selected row vanished (deleted elsewhere, or a catalog entry got added
// under the same name): clear the selection once the store has settled.
watch(
  () => [store.loading, allRows.value] as const,
  ([loading]) => {
    if (loading || route.name !== 'mcp' || !selectedId.value) return
    if (!selectedRow.value) setSelection(null)
  },
)

// ── Data ────────────────────────────────────────────────────────────────────

function refresh() {
  void store.fetchServers()
  void loadGoogleStatus()
}

onMounted(() => {
  store.fetchServers()
  appSettings.ensureLoaded()
  loadGoogleStatus()
})

// Cached by <KeepAlive> (App.vue): onMounted fires once for the app's whole
// lifetime, so re-show the screen with fresh data instead of whatever it read
// the very first time.
useActivatedRefresh(refresh)

// ── Actions ─────────────────────────────────────────────────────────────────

async function handleImport() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'MCP Server', extensions: ['json'] }],
  })
  if (!selected) return
  importing.value = true
  try {
    const server = await store.importServer(selected as string)
    toast.success(t('mcp.list.toast.imported'))
    void select(server.id)
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
    title: t('mcp.list.confirmDelete.title'),
    message: t('mcp.list.confirmDelete.message', { name: server.name }),
    confirmLabel: t('common.delete'),
  }))) return
  deletingId.value = server.id
  try {
    await store.deleteServer(server.id)
    // The delete confirm covers any unsaved edits of the open server.
    if (selectedId.value === server.id) setSelection(null)
    toast.success(t('mcp.list.toast.deleted'))
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
    toast.success(server.enabled ? t('mcp.list.toast.disabled') : t('mcp.list.toast.enabled'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    togglingId.value = null
  }
}

function handleDuplicate(server: McpServer) {
  const seed = duplicateForm(formFromServer(server), copyName(server.name, store.items.map((s) => s.name)))
  void startNew(seed).then((opened) => {
    if (opened && (server.env_keys.length || server.header_keys.length)) toast.info(t('mcp.detail.duplicated'))
  })
}

// One-click add from the catalog: a stub with empty secret values, selected
// right away so the user finishes it in the detail pane.
async function addFromCatalog(item: CatalogEntry) {
  addingName.value = item.name
  try {
    const server = await store.createServer({
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
    void select(server.id)
  } catch (e) {
    toast.error(String(e))
  } finally {
    addingName.value = null
  }
}

function openSettings(row: McpRow) {
  const section = builtins.value.find((b) => b.name === row.name)?.section
  if (section) router.push({ path: '/settings', query: { section } })
}

function onSaved(id: string) {
  if (id !== selectedId.value) setSelection(id)
}

/** Status dot of a row: on, off, or a built-in that needs setup. */
function dotClass(row: McpRow): string {
  if (row.tone === 'warning') return 'bg-amber-500'
  if (row.kind === 'catalog') return 'bg-transparent border border-muted-foreground/40'
  return row.enabled ? 'bg-emerald-500' : 'bg-muted-foreground/40'
}
</script>

<template>
  <div class="flex h-full min-h-0">
    <ListDetailSidebar
      v-model:search="search"
      :groups="groups"
      :selected-id="isNew ? null : selectedId"
      collapse-key="mcp.collapsed"
      :search-placeholder="t('mcp.list.searchPlaceholder')"
      :loading="store.loading"
      :error="store.error"
      :flat-when-single="false"
      @select="select"
      @add="startNew()"
      @retry="refresh"
    >
      <template #toolbar>
        <Button
          variant="outline"
          size="icon-sm"
          class="shrink-0"
          :disabled="importing"
          :title="importing ? t('mcp.list.importing') : t('mcp.list.import')"
          :aria-label="t('mcp.list.import')"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="outline"
          size="icon-sm"
          class="shrink-0"
          :disabled="store.loading"
          :title="t('common.refresh')"
          :aria-label="t('common.refresh')"
          @click="refresh"
        >
          <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="1.75" />
        </Button>
      </template>

      <template #controls>
        <AppSelect v-model="typeFilter" :options="typeOptions" :title="t('mcp.list.filterByType')" />
      </template>

      <template #row="{ row }">
        <span class="flex min-w-0 items-center gap-1.5">
          <span class="h-1.5 w-1.5 shrink-0 rounded-full" :class="dotClass(row)" />
          <span
            class="min-w-0 flex-1 truncate font-mono text-sm font-medium leading-snug"
            :class="row.kind === 'server' && !row.enabled && 'text-muted-foreground'"
          >
            {{ row.name }}
          </span>
          <span
            v-if="row.transport === 'sse' && defaultIsCodex"
            class="shrink-0 rounded bg-amber-500/15 px-1 text-[9px] font-semibold text-amber-600 dark:text-amber-400"
            :title="t('mcp.list.claudeOnlyTitle')"
          >
            {{ t('mcp.list.claudeOnly') }}
          </span>
          <span
            v-if="row.kind === 'catalog'"
            class="shrink-0 rounded bg-muted px-1 text-[9px] font-semibold uppercase tracking-wide text-muted-foreground"
          >
            {{ row.transport }}
          </span>
        </span>
        <span class="mt-1 truncate pl-3 font-mono text-[11px] text-muted-foreground/70" :title="row.line">
          {{ row.line || '—' }}
        </span>
      </template>
    </ListDetailSidebar>

    <McpServerDetail
      v-if="isNew"
      :key="`new-${draftKey}`"
      ref="detailRef"
      :server="null"
      :seed="draftSeed"
      :default-is-codex="defaultIsCodex"
      :toggling="false"
      @saved="onSaved"
    />
    <McpServerDetail
      v-else-if="selectedServer"
      :key="selectedServer.id"
      ref="detailRef"
      :server="selectedServer"
      :default-is-codex="defaultIsCodex"
      :toggling="togglingId === selectedServer.id"
      @saved="onSaved"
      @toggle-enabled="handleToggleEnabled(selectedServer)"
      @export="handleExport(selectedServer)"
      @duplicate="handleDuplicate(selectedServer)"
      @delete="handleDelete(selectedServer)"
    />
    <McpInfoDetail
      v-else-if="selectedRow && selectedRow.kind !== 'server'"
      :key="selectedRow.id"
      :row="selectedRow"
      :entry="selectedEntry"
      :status-label="selectedBuiltin?.status ?? ''"
      :adding="addingName === selectedRow.name"
      @add="selectedEntry && addFromCatalog(selectedEntry)"
      @settings="openSettings(selectedRow)"
    />
    <DetailEmpty
      v-else
      :icon="Server"
      :text="store.items.length === 0 && !store.loading ? t('mcp.list.empty.title') : t('mcp.list.noSelection')"
    >
      <template v-if="store.items.length === 0 && !store.loading">
        <p class="mx-auto mt-1 max-w-60 text-[11px] text-muted-foreground/70">{{ t('mcp.list.empty.hint') }}</p>
        <Button class="mt-4" @click="startNew()">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('common.new') }}
        </Button>
      </template>
    </DetailEmpty>
  </div>
</template>
