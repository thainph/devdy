<script setup lang="ts">
// THE Slack threads window (opened via lib/slackWindow, `?slackWindow=1[&id=]
// [&add=1][&projectId=]`): a Slack-like reader of the threads Devdy fetched from
// Slack and translated into Vietnamese.
//
//   ┌ titlebar (Slack · "Slack threads" · pin · reload) ─────────────────────┐
//   ├ sidebar: search, project filter, + Add │ thread pane: header, banners, │
//   │ workspace → #channel → threads         │ parent · N replies · replies  │
//   ├────────────────────────────────────────┴───────────────────────────────┤
//   └ footer: project of the open thread · ⋯ (Copy ID · Delete)              ┘
//
// It is its own webview, so it reads straight from the DB and resyncs on the
// backend's `slack-threads://changed` (debounced) and `slack-threads://progress`
// events, and on focus (the MCP sidecar writes the DB without emitting events).
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { openUrl } from '@tauri-apps/plugin-opener'
import { AlertCircle, RotateCw, Slack } from 'lucide-vue-next'
import { Button, ConfirmModal, ToastHost } from '@/components/ui'
import DetailWindowTitlebar from '@/components/DetailWindowTitlebar.vue'
import DetailWindowFooter from '@/components/DetailWindowFooter.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import SlackSidebar from '@/components/slack/SlackSidebar.vue'
import SlackThreadPane from '@/components/slack/SlackThreadPane.vue'
import SlackAddDialog from '@/components/slack/SlackAddDialog.vue'
import { useSlackText } from '@/components/slack/useSlackText'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { refreshTray } from '@/lib/tray'
import { createSeenGuard } from '@/lib/slackThreadUtils'
import {
  SLACK_WINDOW_ADD,
  SLACK_WINDOW_OPEN_ROUTE,
  SLACK_WINDOW_SELECT,
  type SlackWindowAdd,
  type SlackWindowOpenRoute,
  type SlackWindowSelect,
} from '@/lib/slackWindow'
import { useProjectsStore } from '@/stores/projects'
import {
  SLACK_THREADS_CHANGED,
  SLACK_THREADS_PROGRESS,
  errorCode,
  useSlackThreadsStore,
  type AddSlackThreadResult,
  type SlackThreadProgress,
  type SlackThreadsChanged,
} from '@/stores/slackThreads'

const { t } = useI18n()
const { toast } = useToast()
const { confirm, state: confirmState } = useConfirm()
const { errorText } = useSlackText()
const store = useSlackThreadsStore()
const projects = useProjectsStore()
const { pinned, togglePin } = useFloatingWindow()

const params = new URLSearchParams(window.location.search)
const initialId = params.get('id') || null
const initialProject = params.get('projectId') || null

// ── Sidebar: search (server-side) + project filter (client-side) ────────────

const NO_PROJECT = '__none__'
const search = ref('')
/** '' = all projects, NO_PROJECT = threads without one, else a project id. */
const projectFilter = ref<string>(initialProject ?? '')

let searchTimer: ReturnType<typeof setTimeout> | null = null
watch(search, (q) => {
  if (searchTimer) clearTimeout(searchTimer)
  searchTimer = setTimeout(() => void store.fetchList(q), 250)
})

const visibleThreads = computed(() =>
  store.threads.filter((th) => {
    if (projectFilter.value === NO_PROJECT) return !th.projectId
    if (projectFilter.value) return th.projectId === projectFilter.value
    return true
  }),
)

const projectOptions = computed(() => {
  const ids = new Set<string>()
  for (const th of store.threads) if (th.projectId) ids.add(th.projectId)
  if (projectFilter.value && projectFilter.value !== NO_PROJECT) ids.add(projectFilter.value)
  const named = [...ids]
    .map((id) => ({ value: id, label: projects.projects.find((p) => p.id === id)?.name ?? id }))
    .sort((a, b) => a.label.localeCompare(b.label))
  return [
    { value: '', label: t('slackThread.sidebar.allProjects') },
    ...named,
    { value: NO_PROJECT, label: t('slackThread.sidebar.noProject') },
  ]
})

/** Store empty with no search: the "nothing yet" hint rather than "no results". */
const emptyStore = computed(() => store.threads.length === 0 && !store.query.trim())

// ── Selection ───────────────────────────────────────────────────────────────

const view = computed(() => store.view)
const progress = computed<SlackThreadProgress | null>(() =>
  store.selectedId ? store.progress[store.selectedId] ?? null : null,
)
/** New-message count captured when the thread was opened (marking it seen clears it). */
const openedNewCount = ref(0)

async function select(id: string | null) {
  renaming.value = false
  openedNewCount.value = id ? store.threads.find((x) => x.id === id)?.newCount ?? 0 : 0
  await store.open(id)
}

// The URL follows the selection — including when the thread is deleted elsewhere
// (event / MCP) and the store drops it — so a reload never reopens a dead id.
watch(
  () => store.selectedId,
  (id) => {
    const url = new URL(window.location.href)
    if (id) url.searchParams.set('id', id)
    else url.searchParams.delete('id')
    url.searchParams.delete('add')
    window.history.replaceState(null, '', url.toString())
  },
)

// Mark seen once a ready thread has been on screen for a second — at most once
// per (thread, newest reply): marking seen emits `changed`, which reloads the
// view, which must not mark it again. The backend does not expose last_seen_ts,
// so a thread never seen (newCount 0) is marked on its first view too.
const seenGuard = createSeenGuard()
let seenTimer: ReturnType<typeof setTimeout> | null = null
watch(
  // A primitive key: a reloaded view with the same values does not re-fire.
  () => (view.value ? `${view.value.id}|${view.value.fetchStatus}|${view.value.lastReplyTs ?? ''}` : ''),
  () => {
    if (seenTimer) clearTimeout(seenTimer)
    seenTimer = null
    const v = view.value
    if (!v || v.fetchStatus !== 'ready' || !v.lastReplyTs) return
    const { id, lastReplyTs } = v
    seenTimer = setTimeout(() => {
      const cur = store.view
      if (cur?.id !== id || cur.fetchStatus !== 'ready' || cur.lastReplyTs !== lastReplyTs) return
      if (!seenGuard.take(id, lastReplyTs)) return
      store.markSeen(id).catch(() => seenGuard.release(id, lastReplyTs))
    }, 1000)
  },
)

// ── Thread actions ──────────────────────────────────────────────────────────

const SHOW_ORIGINAL_KEY = 'slackThreads.showOriginal'
const showOriginal = ref(localStorage.getItem(SHOW_ORIGINAL_KEY) === '1')
function toggleShowOriginal() {
  showOriginal.value = !showOriginal.value
  try {
    localStorage.setItem(SHOW_ORIGINAL_KEY, showOriginal.value ? '1' : '0')
  } catch {
    /* storage unavailable */
  }
}

const refreshing = ref(false)
async function refreshThread() {
  const id = store.selectedId
  if (!id || refreshing.value) return
  refreshing.value = true
  try {
    await store.refresh(id)
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(errorText(code, detail))
  } finally {
    refreshing.value = false
  }
}

const renaming = ref(false)
async function commitRename(title: string) {
  renaming.value = false
  const v = view.value
  if (!v || !title || title === v.title) return
  try {
    await store.rename(v.id, title)
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(t('slackThread.renameFailed', { error: errorText(code, detail) }))
  }
}

function openInSlack() {
  const url = view.value?.threadUrl
  if (url) openUrl(url).catch((e) => toast.error(String(e)))
}

async function onProjectChange(next: string | null) {
  const v = view.value
  if (!v || next === v.projectId) return
  try {
    await store.setProject(v.id, next)
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(errorText(code, detail))
  }
}

async function removeThread() {
  const v = view.value
  if (!v) return
  const ok = await confirm({
    title: t('slackThread.confirm.deleteTitle'),
    message: t('slackThread.confirm.deleteMessage'),
    confirmLabel: t('common.delete'),
  })
  if (!ok) return
  try {
    await store.remove([v.id])
  } catch (e) {
    toast.error(String(e))
  }
}

/** A store error (`"<code>: <detail>"` or plain) as user text. */
function errorLine(raw: string): string {
  const { code, detail } = errorCode(raw)
  return errorText(code, detail)
}

const copiedId = ref(false)
async function copyId() {
  const id = store.selectedId
  if (!id) return
  try {
    await navigator.clipboard.writeText(id)
    copiedId.value = true
    setTimeout(() => (copiedId.value = false), 1500)
    toast.success(t('item.idCopied'))
  } catch {
    /* clipboard unavailable */
  }
}

/** The pop-out has no router: ask the main window to show the MCP settings. */
function openMcp() {
  emit(SLACK_WINDOW_OPEN_ROUTE, { path: '/mcp' } satisfies SlackWindowOpenRoute).catch(() => {})
}

// ── Add dialog ──────────────────────────────────────────────────────────────

const addOpen = ref(params.get('add') === '1')
const addProjectId = ref<string | null>(initialProject)

function openAdd(projectId?: string | null) {
  addProjectId.value =
    projectId !== undefined
      ? projectId
      : projectFilter.value && projectFilter.value !== NO_PROJECT
        ? projectFilter.value
        : null
  addOpen.value = true
}

async function onAdded(res: AddSlackThreadResult) {
  addOpen.value = false
  if (res.status === 'existing') toast.info(t('slackThread.add.existing'))
  else toast.success(t('slackThread.add.added'))
  // Make sure the new row is visible under the current filters.
  const row = store.threads.find((x) => x.id === res.id)
  if (row && !visibleThreads.value.some((x) => x.id === res.id)) projectFilter.value = ''
  await select(res.id)
}

// ── Reload / events ─────────────────────────────────────────────────────────

const reloading = ref(false)
async function reloadAll() {
  reloading.value = true
  try {
    await Promise.all([store.fetchList(), store.reloadView()])
  } finally {
    reloading.value = false
  }
}

let listTimer: ReturnType<typeof setTimeout> | null = null
let viewTimer: ReturnType<typeof setTimeout> | null = null
function scheduleList() {
  if (listTimer) clearTimeout(listTimer)
  listTimer = setTimeout(() => void store.fetchList(), 300)
}
function scheduleView() {
  if (viewTimer) clearTimeout(viewTimer)
  viewTimer = setTimeout(() => void store.reloadView(), 300)
}

const windowTitle = computed(() =>
  view.value ? t('slackThread.window.titleWith', { name: view.value.title }) : t('slackThread.window.title'),
)
watch(
  windowTitle,
  async (value) => {
    document.title = value
    try {
      await getCurrentWindow().setTitle(value)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

function isTypingTarget(el: EventTarget | null): boolean {
  const node = el as HTMLElement | null
  if (!node) return false
  return node.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(node.tagName)
}

// F2 renames the open thread (the rename field handles its own Enter / Esc).
function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (!view.value || renaming.value || addOpen.value || confirmState.open) return
  if (isTypingTarget(e.target)) return
  if (e.key === 'F2') {
    e.preventDefault()
    renaming.value = true
  }
}

let unlisten: UnlistenFn[] = []

onMounted(async () => {
  document.addEventListener('keydown', onKeydown)
  void projects.fetchProjects()
  // Listen first, so progress / changes emitted while the first load is in
  // flight (e.g. the startup re-fetch of legacy threads) are not lost.
  // One at a time, each unlisten kept as soon as it exists: if a later listen()
  // fails, the earlier ones are still torn down on unmount.
  try {
    unlisten.push(
      await listen<SlackThreadsChanged>(SLACK_THREADS_CHANGED, (e) => {
        const p = e.payload
        if (p?.action === 'deleted' && p.id) store.onDeleted(p.id)
        scheduleList()
        if (!p?.id || p.id === store.selectedId) scheduleView()
      }),
    )
    unlisten.push(
      await listen<SlackThreadProgress>(SLACK_THREADS_PROGRESS, (e) => {
        if (e.payload) store.applyProgress(e.payload)
      }),
    )
    unlisten.push(
      await listen<SlackWindowSelect>(SLACK_WINDOW_SELECT, (e) => {
        if (e.payload?.id) void select(e.payload.id)
      }),
    )
    unlisten.push(
      await listen<SlackWindowAdd>(SLACK_WINDOW_ADD, (e) => {
        openAdd(e.payload?.projectId ?? null)
      }),
    )
    // MCP sidecar writes (rename / delete / set_project) emit no Tauri event.
    unlisten.push(
      await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (focused) void reloadAll()
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
  await store.fetchList()
  // An event may already have selected a thread meanwhile; keep it.
  const first = store.selectedId ?? initialId ?? visibleThreads.value[0]?.id ?? null
  if (first && first !== store.selectedId) await select(first)
})

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKeydown)
  for (const timer of [searchTimer, listTimer, viewTimer, seenTimer]) if (timer) clearTimeout(timer)
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div class="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
    <DetailWindowTitlebar
      :icon="Slack"
      :title="t('slackThread.window.title')"
      :pinned="pinned"
      :reloading="reloading"
      @toggle-pin="togglePin"
      @reload="reloadAll"
    />

    <!-- Reloading the open thread failed: its last good copy stays on screen. -->
    <div
      v-if="view && store.viewError"
      class="flex shrink-0 items-center gap-2 border-b border-destructive/30 bg-destructive/10 px-4 py-1.5 text-xs text-foreground"
    >
      <AlertCircle class="h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="2" />
      <span class="min-w-0 flex-1 truncate" :title="store.viewError">{{ errorLine(store.viewError) }}</span>
      <Button variant="outline" size="xs" @click="store.reloadView()">
        <RotateCw class="h-3 w-3" :stroke-width="1.75" />
        {{ t('slackThread.banner.retry') }}
      </Button>
    </div>

    <div class="flex min-h-0 flex-1">
      <SlackSidebar
        v-model:search="search"
        v-model:project="projectFilter"
        :threads="visibleThreads"
        :empty-store="emptyStore"
        :selected-id="store.selectedId"
        :progress="store.progress"
        :project-options="projectOptions"
        :error="store.error ? errorLine(store.error) : null"
        @select="select"
        @add="openAdd()"
        @retry="store.fetchList()"
      />

      <SlackThreadPane
        v-if="view"
        :view="view"
        :progress="progress"
        :show-original="showOriginal"
        :new-count="openedNewCount"
        :renaming="renaming"
        :refreshing="refreshing"
        @start-rename="renaming = true"
        @commit-rename="commitRename"
        @cancel-rename="renaming = false"
        @toggle-original="toggleShowOriginal"
        @refresh="refreshThread"
        @open-in-slack="openInSlack"
        @open-mcp="openMcp"
      />
      <div v-else class="flex min-w-0 flex-1 items-center justify-center p-6">
        <div class="text-center">
          <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
            <Slack class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <template v-if="store.selectedId && store.viewError">
            <p class="max-w-sm text-xs text-destructive">{{ errorLine(store.viewError) }}</p>
            <Button variant="outline" size="xs" class="mt-2" @click="store.reloadView()">
              <RotateCw class="h-3 w-3" :stroke-width="1.75" />
              {{ t('slackThread.banner.retry') }}
            </Button>
          </template>
          <p v-else class="text-xs text-muted-foreground">
            {{ store.selectedId ? t('common.loading') : t('slackThread.noSelection') }}
          </p>
        </div>
      </div>
    </div>

    <DetailWindowFooter
      v-if="view"
      :editable="false"
      :copied-id="copiedId"
      @copy-id="copyId"
      @delete="removeThread"
    >
      <template #left>
        <ProjectSelect :model-value="view.projectId" @update:model-value="onProjectChange" />
      </template>
    </DetailWindowFooter>

    <SlackAddDialog
      :open="addOpen"
      :default-project-id="addProjectId"
      @close="addOpen = false"
      @added="onAdded"
      @open-mcp="openMcp"
    />

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
