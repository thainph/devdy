<script setup lang="ts">
// Batch Board: todos added from the todo screen wait in the queue, grouped by
// project. Run starts one Todo Conductor per project (in that project; its
// workers never leave it), and every batch shows here as its own section: the
// conductor on top, its workers in lanes by what they need from the user
// (answer a prompt, check a stall, review the result). To talk to a conductor,
// open its session. Finished batches go to the history. Any other session
// waiting on the user shows up too, so nothing has to be answered in a separate
// window while this screen is open (see useBoardPresence).
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { openUrl } from '@tauri-apps/plugin-opener'
import { ChevronDown, ChevronRight, History, Inbox, Layers, RefreshCw } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { Badge, Banner, Button, Card, PageHeader } from '@/components/ui'
import BatchGroupSection from '@/components/batch/BatchGroupSection.vue'
import BatchQueuePanel from '@/components/batch/BatchQueuePanel.vue'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useConfirm } from '@/composables/useConfirm'
import { usePrompt } from '@/composables/usePrompt'
import { useToast } from '@/composables/useToast'
import { useBoardPresence } from '@/composables/useBoardPresence'
import { runDisplayName } from '@/lib/tray'
import { useMarkdown } from '@/lib/markdown'
import {
  SOLO_BATCH,
  commitMessageFor,
  type BoardGroup,
  type BoardItem,
  type BoardRunResult,
  type QueuedTodo,
} from '@/lib/batchBoard'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const live = useLiveRunsStore()
const runs = useRunsStore()
const { confirm } = useConfirm()
const { prompt } = usePrompt()
const { toast } = useToast()
const { renderText, loadMarkdown } = useMarkdown()
useBoardPresence()

const groups = ref<BoardGroup[]>([])
/** Todos added with "Add to Batch Board", waiting to be run. */
const queue = ref<QueuedTodo[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const busy = ref(new Set<string>())
const showHistory = ref(false)
/** History batches opened one by one. */
const expanded = ref(new Set<string>())
/** A batch to scroll to and outline (`?batch=` or just started). */
const highlight = ref<string>(typeof route.query.batch === 'string' ? route.query.batch : '')

const active = computed(() => groups.value.filter((g) => g.active))
const history = computed(() => groups.value.filter((g) => !g.active))

/** Sessions outside every batch on screen that wait on the user (answered here too). */
const otherPending = computed(() => {
  const mine = new Set(active.value.flatMap((g) => g.items.map((i) => i.runId)))
  const out: { runId: string; projectId: string }[] = []
  live.sessions.forEach((s, runId) => {
    if (s.permissionQueue.length && !mine.has(runId)) out.push({ runId, projectId: s.projectId })
  })
  return out
})

// Only the newest request may land: an older answer must not overwrite a newer one.
let loadSeq = 0
async function load() {
  const seq = ++loadSeq
  loading.value = true
  try {
    const [nextGroups, nextQueue] = await Promise.all([
      invoke<BoardGroup[]>('board_overview'),
      invoke<QueuedTodo[]>('board_queue_list'),
    ])
    if (seq !== loadSeq) return
    groups.value = nextGroups
    queue.value = nextQueue
    error.value = null
  } catch (e) {
    if (seq === loadSeq) error.value = String(e)
  } finally {
    if (seq === loadSeq) loading.value = false
  }
}

/** Bring a batch into view (opening it in the history if it's there). */
async function reveal(runId: string) {
  if (!runId) return
  if (history.value.some((g) => g.batch.runId === runId)) {
    showHistory.value = true
    expanded.value = new Set(expanded.value).add(runId)
  }
  await nextTick()
  document.getElementById(`batch-${runId}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

/** Conductors started from the queue: show the first one. */
async function onStarted(result: BoardRunResult) {
  await load()
  highlight.value = result.started[0]?.runId ?? ''
  void reveal(highlight.value)
}

watch(() => route.query.batch, (b) => {
  if (typeof b === 'string' && b) {
    highlight.value = b
    void load().then(() => reveal(b))
  }
})

function toggleHistoryGroup(runId: string) {
  const next = new Set(expanded.value)
  if (next.has(runId)) next.delete(runId)
  else next.add(runId)
  expanded.value = next
}

function openRun(item: { runId: string; projectId: string }) {
  void router.push({ name: 'project-run-detail', params: { projectId: item.projectId, runId: item.runId } })
}

async function withBusy(runId: string, fn: () => Promise<void>) {
  busy.value = new Set(busy.value).add(runId)
  try {
    await fn()
  } catch (e) {
    toast.error(String(e))
  } finally {
    const next = new Set(busy.value)
    next.delete(runId)
    busy.value = next
    void load()
  }
}

function stop(item: BoardItem) {
  void withBusy(item.runId, () => runs.cancelRun(item.runId))
}

function commit(item: BoardItem) {
  const wt = item.worktree
  if (!wt) return
  void withBusy(item.runId, async () => {
    const msg = await prompt({
      title: t('board.commitTitle'),
      label: t('board.commitLabel'),
      initialValue: commitMessageFor(item),
      confirmLabel: t('board.commitAll'),
    })
    if (!msg) return
    await invoke('worktree_commit_all', { id: wt.id, message: msg })
    toast.success(t('board.committed'))
  })
}

function push(item: BoardItem) {
  const wt = item.worktree
  if (!wt) return
  void withBusy(item.runId, async () => {
    const url = await invoke<string>('worktree_push', { id: wt.id })
    toast.success(t('board.pushed', { branch: wt.branch }))
    if (url.startsWith('https://')) await openUrl(url)
  })
}

async function finish(item: BoardItem) {
  if (item.diff?.uncommitted && !(await confirm({
    title: t('board.finishTitle'),
    message: t('board.finishUncommitted', { n: item.diff.uncommitted }),
    confirmLabel: t('board.finish'),
  }))) return
  void withBusy(item.runId, async () => {
    await invoke('board_close_item', { runId: item.runId, done: true })
    toast.success(t('board.finished'))
  })
}

async function discard(item: BoardItem) {
  if (!(await confirm({
    title: t('board.discardTitle'),
    message: item.worktree
      ? item.diff?.commits
        ? t('board.discardMessageCommits', { branch: item.worktree.branch, n: item.diff.commits })
        : t('board.discardMessageWorktree', { branch: item.worktree.branch })
      : t('board.discardMessage'),
    confirmLabel: t('board.discard'),
  }))) return
  void withBusy(item.runId, () => invoke('board_close_item', { runId: item.runId, done: false }))
}

// Same answer path as the permission windows.
function decideOther(runId: string, decision: 'allow' | 'deny', remember: boolean) {
  const req = live.get(runId)?.permissionQueue[0]
  if (!req) return
  if (remember && decision === 'allow') live.rememberAllowedTool(runId, req.tool_name, req.request_id)
  else if (remember && decision === 'deny') live.rememberDeniedTool(runId, req.tool_name, req.request_id)
  void live.respondPermission(runId, req, decision, { remember })
}
function answerOther(runId: string, answers: Record<string, string>) {
  const req = live.get(runId)?.permissionQueue[0]
  if (req) void live.respondPermission(runId, req, 'allow', { answers })
}

// Live updates: worktree/todo changes, runs finishing, new workers; plus a slow
// poll for diffs (git changes made by the sessions emit nothing).
const unlisten: UnlistenFn[] = []
let offDone: (() => void) | null = null
let poll: ReturnType<typeof setInterval> | null = null
let reloadTimer: ReturnType<typeof setTimeout> | null = null
function scheduleReload() {
  if (reloadTimer) clearTimeout(reloadTimer)
  reloadTimer = setTimeout(() => void load(), 400)
}
// A worker spawned by a conductor shows up as a new live session.
watch(() => live.sessions.size, scheduleReload)

// Left before the async setup finished: what it registers late is undone at once.
let disposed = false
onMounted(async () => {
  void loadMarkdown()
  await load()
  if (disposed) return
  if (highlight.value) void reveal(highlight.value)
  // One by one: a failed registration must not leak the ones that succeeded.
  for (const event of ['board:changed', 'focus:changed']) {
    try {
      const off = await listen(event, scheduleReload)
      if (disposed) off()
      else unlisten.push(off)
    } catch {
      /* outside the Tauri shell */
    }
  }
  if (disposed) return
  offDone = live.onAnyDone(scheduleReload)
  poll = setInterval(() => void load(), 15000)
})
onBeforeUnmount(() => {
  disposed = true
  unlisten.forEach((off) => off())
  offDone?.()
  if (poll) clearInterval(poll)
  if (reloadTimer) clearTimeout(reloadTimer)
})
</script>

<template>
  <div class="flex flex-col h-full">
    <PageHeader :title="t('board.title')">
      <template #leading>
        <Layers class="h-4 w-4 text-muted-foreground" :stroke-width="1.75" />
      </template>
      <template #actions>
        <Button variant="outline" size="icon" :title="t('board.refresh')" :aria-label="t('board.refresh')" @click="load">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loading }" :stroke-width="1.75" />
        </Button>
      </template>
    </PageHeader>

    <div class="flex-1 min-h-0 overflow-auto px-6 py-4 space-y-4">
      <Banner v-if="error" tone="destructive">{{ error }}</Banner>

      <!-- Other sessions waiting on the user. -->
      <div v-if="otherPending.length" class="space-y-2">
        <p class="text-[11px] font-medium uppercase tracking-wider text-primary">{{ t('board.otherPending') }}</p>
        <div class="grid gap-3 md:grid-cols-2">
          <Card v-for="p in otherPending" :key="p.runId" body-class="p-0">
            <button
              type="button"
              class="flex w-full items-center gap-1.5 px-3 pt-2.5 text-left text-xs font-medium hover:text-primary cursor-pointer"
              @click="openRun(p)"
            >
              <Badge size="xs" tone="neutral">{{ runDisplayName(p.runId, p.projectId).project }}</Badge>
              <span class="truncate">{{ runDisplayName(p.runId, p.projectId).name }}</span>
            </button>
            <PermissionPrompt
              v-if="live.get(p.runId)?.permissionQueue[0]"
              :key="live.get(p.runId)!.permissionQueue[0].request_id"
              :request="live.get(p.runId)!.permissionQueue[0]"
              :allowed-tools="live.get(p.runId)?.allowedTools ?? []"
              :render-text="renderText"
              @decide="(d, r) => decideOther(p.runId, d, r)"
              @answer="(a) => answerOther(p.runId, a)"
            />
          </Card>
        </div>
      </div>

      <!-- Queued todos, by project: run them. -->
      <BatchQueuePanel v-if="queue.length" :queue="queue" @started="onStarted" @changed="load" />

      <!-- Nothing queued or run yet. -->
      <div v-if="!groups.length && !queue.length && !loading" class="flex flex-col items-center justify-center gap-3 py-20 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
          <Inbox class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('board.emptyTitle') }}</p>
        <p class="max-w-md text-xs text-muted-foreground">{{ t('board.emptyBody') }}</p>
      </div>

      <!-- Batches in progress / waiting for review: one section per project run. -->
      <BatchGroupSection
        v-for="g in active"
        :id="`batch-${g.batch.runId}`"
        :key="g.batch.runId"
        :group="g"
        :busy="busy"
        :highlighted="highlight === g.batch.runId"
        @open="openRun"
        @stop="stop"
        @commit="commit"
        @push="push"
        @finish="finish"
        @discard="discard"
      />
      <p v-if="groups.length && !active.length" class="py-6 text-center text-xs text-muted-foreground">
        {{ t('board.nothingActive') }}
      </p>

      <!-- Finished batches. -->
      <div v-if="history.length" class="space-y-2">
        <button
          type="button"
          class="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wider text-muted-foreground hover:text-foreground cursor-pointer"
          :aria-expanded="showHistory"
          @click="showHistory = !showHistory"
        >
          <component :is="showHistory ? ChevronDown : ChevronRight" class="h-3.5 w-3.5" :stroke-width="2" />
          <History class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('board.history', { n: history.length }) }}
        </button>
        <template v-if="showHistory">
          <div v-for="g in history" :id="`batch-${g.batch.runId}`" :key="g.batch.runId">
            <BatchGroupSection
              v-if="expanded.has(g.batch.runId)"
              :group="g"
              :busy="busy"
              :highlighted="highlight === g.batch.runId"
              @open="openRun"
              @stop="stop"
              @commit="commit"
              @push="push"
              @finish="finish"
              @discard="discard"
            />
            <button
              type="button"
              class="flex w-full items-center gap-2 rounded-md px-3 py-1.5 text-left text-xs hover:bg-muted/50 cursor-pointer"
              :class="expanded.has(g.batch.runId) ? 'mt-1 text-muted-foreground' : ''"
              @click="toggleHistoryGroup(g.batch.runId)"
            >
              <component :is="expanded.has(g.batch.runId) ? ChevronDown : ChevronRight" class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
              <template v-if="g.batch.runId === SOLO_BATCH">
                <span class="truncate">{{ t('board.solo') }}</span>
              </template>
              <template v-else>
                <Badge size="xs" tone="neutral" class="shrink-0">{{ g.batch.projectName }}</Badge>
                <span class="min-w-0 truncate">{{ (g.batch.title ?? '').replace(/^🗂\s*/, '') || t('board.untitled') }}</span>
                <span class="ml-auto shrink-0 text-[10px] text-muted-foreground">{{ new Date(g.batch.createdAt).toLocaleString() }}</span>
              </template>
            </button>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>
