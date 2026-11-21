<script setup lang="ts">
// THE Slack threads window (opened via lib/slackWindow, `?slackWindow=1[&id=]
// [&add=1][&projectId=]`): a Slack-like reader of the threads Devdy fetched from
// Slack and translated into Vietnamese.
//
//   ┌ (native OS titlebar only — no custom titlebar row, to stay compact) ───┐
//   ├ sidebar: search · pin · reload,         │ thread pane header: title /   │
//   │ project filter + Add, activity filter,  │ #channel · [project][Sessions]│
//   │ workspace → #channel → threads          │                               │
//   │                                         │ | Original · Refresh · Slack ·  │
//   │                                         │ ⋯ (New session from this      │
//   │                                         │ thread · Copy ID · Delete)    │
//   │                                         │ banners, parent · N replies · │
//   │                                         │ replies                       │
//   └─────────────────────────────────────────┴───────────────────────────────┘
//
// No footer: both panes run to the bottom of the window.
//
// The thread's project sits next to its "Sessions" menu (linked sessions +
// "Link a session"), since what can be linked depends on the project.
//
// It is its own webview, so it reads straight from the DB and resyncs on the
// backend's `slack-threads://changed` (debounced) and `slack-threads://progress`
// events, and on focus (the MCP sidecar writes the DB without emitting events).
//
// Sessions (runs) live in the main window: opening a linked session or starting
// a new one from the thread is a request to it (WINDOW_OPEN_RUN /
// WINDOW_NEW_SESSION of lib/sessionLinks, handled in App.vue).
//
// `embedded`: rendered as a tab of the main window instead. The props then
// replace the URL params (id / projectId / add) and a new `initialId` selects
// that thread like SLACK_WINDOW_SELECT does; it fills its parent, leaves the
// window's title, tray row, pin and URL alone, and uses the main window's
// ConfirmModal / ToastHost. The requests above (and SLACK_WINDOW_OPEN_ROUTE)
// are global events, so they reach the main window's listeners from here too.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { openUrl } from '@tauri-apps/plugin-opener'
import { RotateCw, Slack } from 'lucide-vue-next'
import { Banner, Button, ConfirmModal, ToastHost } from '@/components/ui'
import SlackSidebar from '@/components/slack/SlackSidebar.vue'
import SlackThreadPane from '@/components/slack/SlackThreadPane.vue'
import SlackAddDialog from '@/components/slack/SlackAddDialog.vue'
import { useSlackText } from '@/components/slack/useSlackText'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { requestDock } from '@/lib/surfaceDock'
import { refreshTray } from '@/lib/tray'
import { projectOptionsForIds } from '@/lib/projectOptions'
import { createSeenGuard, isRecent, isUnread, matchesActivity, type ActivityFilter } from '@/lib/slackThreadUtils'
import {
  SLACK_WINDOW_ADD,
  SLACK_WINDOW_OPEN_ROUTE,
  SLACK_WINDOW_SELECT,
  type SlackWindowAdd,
  type SlackWindowOpenRoute,
  type SlackWindowSelect,
} from '@/lib/slackWindow'
import {
  WINDOW_NEW_SESSION,
  WINDOW_OPEN_RUN,
  linkReference,
  type SessionLink,
  type WindowNewSession,
  type WindowOpenRun,
} from '@/lib/sessionLinks'
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

const props = defineProps<{
  /** A tab of the main window, not its own OS window. */
  embedded?: boolean
  /** Thread to select (overrides the URL's `id`); a new value selects that thread. */
  initialId?: string | null
  /** Project to pre-filter the sidebar with (overrides the URL's `projectId`). */
  projectId?: string | null
  /** Open the add dialog on mount (overrides the URL's `add`). */
  add?: boolean
}>()

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const { errorText } = useSlackText()
const store = useSlackThreadsStore()
const projects = useProjectsStore()
// Embedded, the window is the main one: never float / pin it.
const { pinned, togglePin } = props.embedded ? { pinned: ref(false), togglePin: () => {} } : useFloatingWindow()

const params = new URLSearchParams(window.location.search)
const initialId = props.initialId !== undefined ? props.initialId : params.get('id') || null
const initialProject = props.projectId !== undefined ? props.projectId : params.get('projectId') || null

// ── Sidebar: search (server-side) + project / activity filters (client-side) 

const NO_PROJECT = '__none__'
const search = ref('')
/** '' = all projects, NO_PROJECT = threads without one, else a project id. */
const projectFilter = ref<string>(initialProject ?? '')
/** '' = all, else only unread / recently updated threads. */
const activityFilter = ref<ActivityFilter>('')

let searchTimer: ReturnType<typeof setTimeout> | null = null
watch(search, (q) => {
  if (searchTimer) clearTimeout(searchTimer)
  searchTimer = setTimeout(() => void store.fetchList(q), 250)
})

function inProject(th: { projectId: string | null }): boolean {
  if (projectFilter.value === NO_PROJECT) return !th.projectId
  return !projectFilter.value || th.projectId === projectFilter.value
}

const visibleThreads = computed(() => {
  const now = Date.now()
  return store.threads.filter(
    (th) =>
      inProject(th) &&
      // The open thread stays put: opening it marks it read, which would drop it from "Unread".
      (th.id === store.selectedId || matchesActivity(th, activityFilter.value, now)),
  )
})

/** Activity tab counts: the project in scope, before the activity filter. */
const activityCounts = computed(() => {
  const now = Date.now()
  const inScope = store.threads.filter(inProject)
  return {
    all: inScope.length,
    unread: inScope.filter((th) => isUnread(th)).length,
    recent: inScope.filter((th) => isRecent(th, now)).length,
  }
})

const projectOptions = computed(() => {
  const ids = new Set<string>()
  for (const th of store.threads) if (th.projectId) ids.add(th.projectId)
  if (projectFilter.value && projectFilter.value !== NO_PROJECT) ids.add(projectFilter.value)
  const named = projectOptionsForIds(ids, (id) => projects.projects.find((p) => p.id === id)?.name)
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
// Not the main window's URL: that one is the router's.
watch(
  () => store.selectedId,
  (id) => {
    if (props.embedded) return
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
async function refreshThread(retranslate = false) {
  const id = store.selectedId
  if (!id || refreshing.value) return
  refreshing.value = true
  try {
    await (retranslate ? store.retranslate(id) : store.refresh(id))
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

// ── Linked sessions ─────────────────────────────────────────────────────────

/** Show a linked session: its pop-out if it has one, else the main window. */
function openRun(link: SessionLink) {
  emit(WINDOW_OPEN_RUN, {
    projectId: link.projectId,
    runId: link.runId,
  } satisfies WindowOpenRun).catch(() => {})
}

async function unlinkRun(link: SessionLink) {
  const v = view.value
  if (!v) return
  try {
    await store.unlinkRun(v.id, link.runId)
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(t('sessionLink.unlinkFailed', { error: errorText(code, detail) }))
  }
}

async function linkSession(runId: string) {
  const v = view.value
  if (!v) return
  try {
    await store.linkRun(v.id, runId, 'manual')
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(t('sessionLink.linkFailed', { error: errorText(code, detail) }))
  }
}

/**
 * Ask the main window for a new session in the thread's project, its composer
 * prefilled with the thread reference; it links the thread once the run exists.
 */
function newSession() {
  const v = view.value
  if (!v) return
  if (!v.projectId) {
    toast.info(t('slackThread.links.needProject'))
    return
  }
  emit(WINDOW_NEW_SESSION, {
    projectId: v.projectId,
    text: linkReference('slack', v.id, v.title.trim() || t('slackThread.untitled')),
    target: { kind: 'slack', id: v.id },
  } satisfies WindowNewSession).catch((e) => toast.error(String(e)))
}

/** The pop-out has no router: ask the main window to show the MCP settings. */
function openMcp() {
  emit(SLACK_WINDOW_OPEN_ROUTE, { path: '/mcp' } satisfies SlackWindowOpenRoute).catch(() => {})
}

// ── Add dialog ──────────────────────────────────────────────────────────────

const addOpen = ref(props.embedded ? !!props.add : props.add || params.get('add') === '1')
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
  if (row && !visibleThreads.value.some((x) => x.id === res.id)) {
    projectFilter.value = ''
    activityFilter.value = ''
  }
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
    if (props.embedded) return
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

// The host re-points the tab: another thread (the SLACK_WINDOW_SELECT path),
// another project scope, or the add dialog (SLACK_WINDOW_ADD).
watch(
  () => props.initialId,
  (id) => {
    if (id) void select(id)
  },
)
watch(
  () => props.projectId,
  (projectId) => {
    if (projectId !== undefined) projectFilter.value = projectId ?? ''
  },
)
watch(
  () => props.add,
  (add) => {
    if (add) openAdd(props.projectId ?? null)
  },
)

/** Pop-out → a tab of the main window, at the same thread. */
function dock() {
  const at: Record<string, string> = {}
  if (store.selectedId) at.id = store.selectedId
  if (projectFilter.value && projectFilter.value !== NO_PROJECT) at.projectId = projectFilter.value
  void requestDock('slack', at)
}

let unlisten: UnlistenFn[] = []

onMounted(async () => {
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
  // An event may already have selected a thread meanwhile; keep it. Embedded,
  // the store is the main window's and may still hold a previous tab's thread.
  const first =
    (props.embedded ? initialId : null) ?? store.selectedId ?? initialId ?? visibleThreads.value[0]?.id ?? null
  if (first && first !== store.selectedId) await select(first)
})

onBeforeUnmount(() => {
  for (const timer of [searchTimer, listTimer, viewTimer, seenTimer]) if (timer) clearTimeout(timer)
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div
    class="flex flex-col overflow-hidden bg-background text-foreground"
    :class="embedded ? 'h-full min-h-0 w-full' : 'app-shell app-window h-screen w-screen'"
  >
    <!-- Reloading the open thread failed: its last good copy stays on screen. -->
    <Banner
      v-if="view && store.viewError"
      tone="destructive"
      class="shrink-0 items-center rounded-none border-x-0 border-t-0 px-4 text-xs"
    >
      <p class="truncate" :title="store.viewError">{{ errorLine(store.viewError) }}</p>
      <template #actions>
        <Button variant="outline" size="xs" @click="store.reloadView()">
          <RotateCw class="h-3 w-3" :stroke-width="1.75" />
          {{ t('slackThread.banner.retry') }}
        </Button>
      </template>
    </Banner>

    <div class="flex min-h-0 flex-1">
      <SlackSidebar
        v-model:search="search"
        v-model:project="projectFilter"
        v-model:activity="activityFilter"
        :threads="visibleThreads"
        :activity-counts="activityCounts"
        :empty-store="emptyStore"
        :selected-id="store.selectedId"
        :progress="store.progress"
        :project-options="projectOptions"
        :error="store.error ? errorLine(store.error) : null"
        :pinned="pinned"
        :embedded="embedded"
        :reloading="reloading"
        @select="select"
        @add="openAdd()"
        @retry="store.fetchList()"
        @toggle-pin="togglePin"
        @dock="dock"
        @reload="reloadAll"
      />

      <SlackThreadPane
        v-if="view"
        :view="view"
        :links="store.links"
        :progress="progress"
        :show-original="showOriginal"
        :new-count="openedNewCount"
        :renaming="renaming"
        :refreshing="refreshing"
        :copied-id="copiedId"
        @start-rename="renaming = true"
        @commit-rename="commitRename"
        @cancel-rename="renaming = false"
        @toggle-original="toggleShowOriginal"
        @refresh="refreshThread()"
        @retranslate="refreshThread(true)"
        @open-in-slack="openInSlack"
        @open-mcp="openMcp"
        @open-run="openRun"
        @unlink-run="unlinkRun"
        @link-run="linkSession"
        @new-session="newSession"
        @copy-id="copyId"
        @delete="removeThread"
        @set-project="onProjectChange"
      />
      <div v-else class="flex min-w-0 flex-1 items-center justify-center p-6">
        <div class="text-center">
          <div class="mx-auto mb-3 flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
            <Slack class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
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

    <SlackAddDialog
      :open="addOpen"
      :default-project-id="addProjectId"
      @close="addOpen = false"
      @added="onAdded"
      @open-mcp="openMcp"
    />

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <template v-if="!embedded">
      <ConfirmModal />
      <ToastHost />
    </template>
  </div>
</template>
