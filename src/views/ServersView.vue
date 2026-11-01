<script setup lang="ts">
// Servers screen, laid out like the Notes window: the sidebar lists the VPS
// servers (search, tag filter, folders by first tag) and the
// right pane edits the selected one inline (ServerDetail). The selection lives
// in the URL query — `?id=<id>` a server, `?new=1` a blank draft — so a
// reload reopens it.
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { HardDrive, RotateCw } from 'lucide-vue-next'
import { AppSelect, Button } from '@/components/ui'
import ListDetailSidebar from '@/components/listDetail/ListDetailSidebar.vue'
import DetailEmpty from '@/components/listDetail/DetailEmpty.vue'
import ServerDetail, { type ServerPrefill } from '@/components/servers/ServerDetail.vue'
import { useActivatedRefresh } from '@/composables/useActivatedRefresh'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import {
  allServerTags,
  filterServers,
  groupServers,
  summarizeTests,
} from '@/lib/serverRows'
import { useServersStore, type TestConnectionResult, type VpsServer } from '@/stores/servers'

// Named explicitly: <KeepAlive :include> in App.vue matches on component name,
// and an inferred name is a build detail that minification can change.
defineOptions({ name: 'ServersView' })

const route = useRoute()
const router = useRouter()
const store = useServersStore()
const { confirm } = useConfirm()
const { toast } = useToast()
const { t } = useI18n()

// ── Sidebar ─────────────────────────────────────────────────────────────────

const search = ref('')
const tagFilter = ref('')

const tags = computed(() => allServerTags(store.items))
const tagOptions = computed(() => [
  { value: '', label: t('servers.list.allTags') },
  ...tags.value.map((tag) => ({ value: tag, label: tag })),
])
// The filtered tag left with its last server: show everything again.
watch(tags, (list) => {
  if (tagFilter.value && !list.includes(tagFilter.value)) tagFilter.value = ''
})

const groups = computed(() =>
  groupServers(
    filterServers(store.items, { search: search.value, tag: tagFilter.value }),
    t('servers.list.untagged'),
  ),
)

// ── Selection (URL query) ───────────────────────────────────────────────────

type Selection = { kind: 'none' } | { kind: 'server'; id: string } | { kind: 'new' }

const selection = ref<Selection>({ kind: 'none' })
/** Bumped per blank draft so ServerDetail remounts with a fresh form. */
const draftSeq = ref(0)
const draftPrefill = ref<ServerPrefill | null>(null)
const detailRef = ref<InstanceType<typeof ServerDetail> | null>(null)

function selectionQuery(sel: Selection): Record<string, string> {
  if (sel.kind === 'server') return { id: sel.id }
  if (sel.kind === 'new') return { new: '1' }
  return {}
}

function sameSelection(a: Selection, b: Selection): boolean {
  return a.kind === b.kind && (a.kind !== 'server' || a.id === (b as { id: string }).id)
}

// In-screen moves (already confirmed) set the selection first, then the URL,
// so the query watcher sees nothing new and only outside changes reach it.
function navigate(sel: Selection) {
  selection.value = sel
  void router.replace({ query: selectionQuery(sel) })
}

// Only this route's query: a kept-alive screen still sees other routes'.
watch(
  () => (route.name === 'servers' ? route.query : null),
  async (query, before) => {
    if (!query) return
    // Back from another screen via a bare /servers link: keep what was open
    // (and any unsaved draft) rather than dropping it.
    if (before === null && Object.keys(query).length === 0 && selection.value.kind !== 'none') {
      void router.replace({ query: selectionQuery(selection.value) })
      return
    }
    const id = typeof query.id === 'string' ? query.id : ''
    const next: Selection = id ? { kind: 'server', id } : query.new === '1' ? { kind: 'new' } : { kind: 'none' }
    if (sameSelection(next, selection.value)) return
    // An outside ?id= / ?new=1 (e.g. a redirect) must not drop a dirty draft silently.
    if (!(await confirmDiscard())) {
      void router.replace({ query: selectionQuery(selection.value) })
      return
    }
    if (next.kind === 'new') {
      draftPrefill.value = null
      draftSeq.value++
    }
    selection.value = next
  },
  { immediate: true },
)

const selectedId = computed(() => (selection.value.kind === 'server' ? selection.value.id : null))
const selectedServer = computed(() =>
  selectedId.value ? store.items.find((s) => s.id === selectedId.value) ?? null : null,
)
const detailKey = computed(() =>
  selection.value.kind === 'server' ? selection.value.id : `new-${draftSeq.value}`,
)

/** Leaving unsaved edits takes a confirm; true = go ahead. */
async function confirmDiscard(): Promise<boolean> {
  if (!detailRef.value?.dirty) return true
  return confirm({
    title: t('servers.detail.discardTitle'),
    message: t('servers.detail.discardMessage'),
    confirmLabel: t('servers.detail.discard'),
  })
}

async function select(id: string) {
  if (id === selectedId.value) return
  if (!(await confirmDiscard())) return
  navigate({ kind: 'server', id })
}

async function openDraft(prefill: ServerPrefill | null) {
  if (!(await confirmDiscard())) return
  draftPrefill.value = prefill
  draftSeq.value++
  navigate({ kind: 'new' })
}

function duplicate() {
  const s = selectedServer.value
  if (!s) return
  // No key file / passphrase: those are per-server secrets.
  void openDraft({
    label: t('servers.detail.copyLabel', { label: s.label }),
    host: s.host,
    port: s.port,
    username: s.username,
    auth_method: s.auth_method,
    tags: s.tags,
  })
}

function onSaved(server: VpsServer, created: boolean) {
  if (created) navigate({ kind: 'server', id: server.id })
}

// ── Load ────────────────────────────────────────────────────────────────────

const loaded = ref(false)

async function refresh() {
  await store.fetchServers()
  loaded.value = true
}

onMounted(() => {
  void refresh()
})

// Cached by <KeepAlive> (App.vue): onMounted fires once for the app's whole
// lifetime, so re-show the screen with fresh data instead of whatever it read
// the very first time.
useActivatedRefresh(() => refresh())

// A stale ?id= (deleted here or elsewhere) clears the selection.
watch([selectedId, selectedServer, loaded], () => {
  if (route.name !== 'servers' || !loaded.value || store.loading || store.error) return
  if (selectedId.value && !selectedServer.value) navigate({ kind: 'none' })
})

// ── Tests ───────────────────────────────────────────────────────────────────

/** Last result per server id, while the screen lives. */
const testResults = reactive(new Map<string, TestConnectionResult>())
const testingIds = reactive(new Set<string>())
const testingAll = ref(false)

async function runTest(id: string): Promise<TestConnectionResult> {
  testingIds.add(id)
  try {
    const result = await store.testConnection(id)
    testResults.set(id, result)
    return result
  } catch (e) {
    const result = { ok: false, message: String(e) }
    testResults.set(id, result)
    throw e
  } finally {
    testingIds.delete(id)
  }
}

async function testSelected() {
  if (!selectedId.value) return
  // The detail pane shows the outcome inline.
  await runTest(selectedId.value).catch(() => {})
}

async function testAll() {
  if (testingAll.value || store.items.length === 0) return
  testingAll.value = true
  try {
    const results = await Promise.allSettled(store.items.map((s) => runTest(s.id)))
    const { online, offline } = summarizeTests(results)
    const message = t('servers.list.toast.testAllSummary', { online, offline })
    if (offline === 0) toast.success(message)
    else toast.error(message)
  } finally {
    testingAll.value = false
  }
}

// ── Delete ──────────────────────────────────────────────────────────────────

const deletingId = ref<string | null>(null)

async function handleDelete(server: VpsServer) {
  if (!(await confirm({
    title: t('servers.list.confirmDelete.title'),
    message: t('servers.list.confirmDelete.message', { label: server.label }),
    confirmLabel: t('common.delete'),
  }))) return
  deletingId.value = server.id
  try {
    await store.deleteServer(server.id)
    testResults.delete(server.id)
    if (selectedId.value === server.id) navigate({ kind: 'none' })
    toast.success(t('servers.list.toast.deleted'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    deletingId.value = null
  }
}
</script>

<template>
  <div class="flex h-full min-h-0">
    <ListDetailSidebar
      v-model:search="search"
      :groups="groups"
      :selected-id="selectedId"
      collapse-key="devdy.servers.collapsedGroups"
      :search-placeholder="t('servers.list.searchPlaceholder')"
      :loading="store.loading && !loaded"
      :error="store.error"
      :empty-store="loaded && store.items.length === 0"
      :empty-title="t('servers.list.empty.title')"
      :empty-hint="t('servers.list.empty.hint')"
      @select="select"
      @add="openDraft(null)"
      @retry="refresh"
    >
      <template #toolbar>
        <Button
          variant="outline"
          size="icon"
          class="shrink-0"
          :disabled="testingAll || store.items.length === 0"
          :title="t('servers.list.testAll')"
          :aria-label="t('servers.list.testAll')"
          @click="testAll"
        >
          <RotateCw class="h-3.5 w-3.5" :class="testingAll && 'animate-spin'" :stroke-width="1.75" />
        </Button>
      </template>
      <template #controls>
        <AppSelect v-model="tagFilter" size="sm" :options="tagOptions" />
      </template>
      <template #row="{ row }">
        <span class="min-w-0 truncate text-sm font-medium leading-snug">{{ row.label }}</span>
        <span class="mt-1 truncate font-mono text-[11px] text-muted-foreground/70">
          {{ row.username }}@{{ row.host }}:{{ row.port }}
        </span>
      </template>
    </ListDetailSidebar>

    <ServerDetail
      v-if="selection.kind === 'new' || selectedServer"
      ref="detailRef"
      :key="detailKey"
      :server="selection.kind === 'new' ? null : selectedServer"
      :prefill="selection.kind === 'new' ? draftPrefill : null"
      :test-result="selectedId ? testResults.get(selectedId) ?? null : null"
      :testing="!!selectedId && testingIds.has(selectedId)"
      :deleting="!!selectedId && deletingId === selectedId"
      @saved="onSaved"
      @test="testSelected"
      @delete="selectedServer && handleDelete(selectedServer)"
      @duplicate="duplicate"
    />
    <DetailEmpty v-else :icon="HardDrive" :text="t('servers.list.noSelection')" />
  </div>
</template>
