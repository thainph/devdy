<script setup lang="ts">
// Batch Board: one screen for a todo batch run by a Todo Conductor. Workers are
// sorted into lanes by what they need from the user (answer a prompt, check a
// stall, review the result), prompts are answered on the cards, and the
// conductor can be talked to from the top bar. Any other session waiting on the
// user shows up too, so nothing has to be answered in a separate window while
// this screen is open (see useBoardPresence).
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Bot, Inbox, Layers, RefreshCw, Send } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { AppSelect, Badge, Banner, Button, Card, Input, PageHeader, StatusBadge } from '@/components/ui'
import BatchBoardCard from '@/components/batch/BatchBoardCard.vue'
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
  commitMessageFor,
  groupByLane,
  lastActivity,
  type BatchSummary,
  type BoardData,
  type BoardItem,
  type Lane,
  type LiveState,
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

const batches = ref<BatchSummary[]>([])
const batchId = ref<string>(typeof route.query.batch === 'string' ? route.query.batch : '')
const data = ref<BoardData>({ items: [], queued: [] })
const loading = ref(false)
const error = ref<string | null>(null)
const busy = ref(new Set<string>())
const conductorMessage = ref('')

const batchOptions = computed(() =>
  batches.value.map((b) => ({
    value: b.runId,
    label: b.title || t('board.untitled'),
    description: `${b.projectName} · ${new Date(b.createdAt).toLocaleString()}`,
  })),
)

const conductor = computed(() => data.value.items.find((i) => i.role === 'conductor') ?? null)
const conductorLive = computed(() => (conductor.value ? live.get(conductor.value.runId) : undefined))
const conductorStatus = computed(() => conductorLive.value?.status || conductor.value?.status || '')
const conductorActivity = computed(() => lastActivity(conductorLive.value?.entries ?? [], 400))

function liveOf(runId: string): LiveState | undefined {
  const s = live.get(runId)
  if (!s) return undefined
  return { pending: s.permissionQueue.length, attention: s.attention.length, status: s.status || undefined }
}
const lanes = computed(() => groupByLane(data.value.items, liveOf))

/** Board columns: what needs the user first. */
const columns = computed(() => [
  { key: 'needs', lanes: ['needs', 'alert'] as Lane[], tone: 'text-primary' },
  { key: 'running', lanes: ['running'] as Lane[], tone: 'text-info' },
  { key: 'review', lanes: ['review'] as Lane[], tone: 'text-success' },
  { key: 'closed', lanes: ['failed', 'closed'] as Lane[], tone: 'text-muted-foreground' },
])
function columnItems(col: { lanes: Lane[] }) {
  return col.lanes.flatMap((l) => lanes.value[l].map((item) => ({ item, lane: l })))
}

/** Sessions outside this batch that wait on the user (answered here too). */
const otherPending = computed(() => {
  const mine = new Set(data.value.items.map((i) => i.runId))
  const out: { runId: string; projectId: string }[] = []
  live.sessions.forEach((s, runId) => {
    if (s.permissionQueue.length && !mine.has(runId)) out.push({ runId, projectId: s.projectId })
  })
  return out
})

async function loadBatches() {
  batches.value = await invoke<BatchSummary[]>('board_batches')
  if (!batchId.value || !batches.value.some((b) => b.runId === batchId.value)) {
    batchId.value = batches.value[0]?.runId ?? ''
  }
}

// Only the newest request may land: switching batches quickly must not let an
// older answer overwrite the board of the batch now shown.
let loadSeq = 0
async function loadItems() {
  const seq = ++loadSeq
  if (!batchId.value) {
    data.value = { items: [], queued: [] }
    return
  }
  loading.value = true
  try {
    const next = await invoke<BoardData>('board_items', { batchRunId: batchId.value })
    if (seq !== loadSeq) return
    data.value = next
    error.value = null
  } catch (e) {
    if (seq === loadSeq) error.value = String(e)
  } finally {
    if (seq === loadSeq) loading.value = false
  }
}

async function refresh() {
  try {
    await loadBatches()
  } catch (e) {
    error.value = String(e)
  }
  await loadItems()
}

watch(batchId, (id) => {
  if (route.query.batch !== id) void router.replace({ query: { ...route.query, batch: id || undefined } })
  void loadItems()
})
watch(() => route.query.batch, (b) => {
  if (typeof b === 'string' && b !== batchId.value) {
    batchId.value = b
    void refresh()
  }
})

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
    void loadItems()
  }
}

/** A follow-up turn, the way RunView sends one: resume a stopped session first. */
async function sendTo(runId: string, projectId: string, text: string) {
  const wasRunning = live.get(runId)?.status === 'running'
  live.pushUser(runId, projectId, text)
  if (!wasRunning) {
    live.setStatus(runId, 'running')
    await live.startListening(runId, projectId)
    await runs.resumeRun(runId)
  }
  await runs.sendUserMessage(runId, text)
}

async function message(item: BoardItem) {
  const text = await prompt({ title: t('board.messageTitle'), label: t('board.messageLabel'), confirmLabel: t('board.send') })
  if (!text) return
  await withBusy(item.runId, () => sendTo(item.runId, item.projectId, text))
}

async function sendToConductor() {
  const text = conductorMessage.value.trim()
  if (!text || !conductor.value) return
  const { runId, projectId } = conductor.value
  conductorMessage.value = ''
  await withBusy(runId, () => sendTo(runId, projectId, text))
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
    message: item.worktree ? t('board.discardMessageWorktree', { branch: item.worktree.branch }) : t('board.discardMessage'),
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
let unlisten: UnlistenFn[] = []
let offDone: (() => void) | null = null
let poll: ReturnType<typeof setInterval> | null = null
let reloadTimer: ReturnType<typeof setTimeout> | null = null
function scheduleReload() {
  if (reloadTimer) clearTimeout(reloadTimer)
  reloadTimer = setTimeout(() => void loadItems(), 400)
}
// A worker spawned by the conductor shows up as a new live session.
watch(() => live.sessions.size, scheduleReload)

onMounted(async () => {
  void loadMarkdown()
  await refresh()
  unlisten = await Promise.all([
    listen('board:changed', () => void refresh()),
    listen('focus:changed', scheduleReload),
  ])
  offDone = live.onAnyDone(scheduleReload)
  poll = setInterval(() => void loadItems(), 15000)
})
onBeforeUnmount(() => {
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
        <div v-if="batchOptions.length" class="w-72">
          <AppSelect v-model="batchId" :options="batchOptions" size="sm" />
        </div>
        <Button variant="outline" size="icon" :title="t('board.refresh')" :aria-label="t('board.refresh')" @click="refresh">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loading }" :stroke-width="1.75" />
        </Button>
      </template>
    </PageHeader>

    <div class="flex-1 min-h-0 overflow-auto">
      <Banner v-if="error" tone="destructive" class="mx-6 mt-4">{{ error }}</Banner>

      <!-- Other sessions waiting on the user. -->
      <div v-if="otherPending.length" class="px-6 pt-4 space-y-2">
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

      <!-- No batch yet. -->
      <div v-if="!batches.length && !loading" class="flex flex-col items-center justify-center gap-3 px-6 py-20 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
          <Inbox class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('board.emptyTitle') }}</p>
        <p class="max-w-md text-xs text-muted-foreground">{{ t('board.emptyBody') }}</p>
      </div>

      <template v-else-if="batchId">
        <!-- The conductor: status, latest word, talk to it. -->
        <div v-if="conductor" class="px-6 pt-4">
          <Card body-class="p-3 space-y-2">
            <div class="flex items-center gap-2 min-w-0">
              <Bot class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
              <button
                type="button"
                class="truncate text-[13px] font-medium hover:text-primary cursor-pointer"
                @click="openRun(conductor)"
              >
                {{ t('board.conductor') }} · {{ conductor.projectName }}
              </button>
              <StatusBadge :status="conductorStatus" size="xs" run-type="session" />
              <span class="ml-auto text-[11px] text-muted-foreground">
                {{ t('board.summary', { workers: data.items.length - 1, queued: data.queued.length }) }}
              </span>
            </div>
            <p v-if="conductorActivity" class="text-xs text-muted-foreground line-clamp-3 whitespace-pre-line">{{ conductorActivity }}</p>
            <div v-if="conductorLive?.permissionQueue[0]" class="-mx-3 border-y border-border/60 bg-muted/30">
              <PermissionPrompt
                :key="conductorLive.permissionQueue[0].request_id"
                :request="conductorLive.permissionQueue[0]"
                :allowed-tools="conductorLive.allowedTools"
                :render-text="renderText"
                @decide="(d, r) => decideOther(conductor!.runId, d, r)"
                @answer="(a) => answerOther(conductor!.runId, a)"
              />
            </div>
            <form class="flex gap-2" @submit.prevent="sendToConductor">
              <Input v-model="conductorMessage" size="sm" class="flex-1" :placeholder="t('board.conductorPlaceholder')" />
              <Button type="submit" size="sm" :disabled="!conductorMessage.trim() || busy.has(conductor.runId)">
                <Send class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('board.send') }}
              </Button>
            </form>
          </Card>
        </div>

        <!-- Lanes. -->
        <div class="grid gap-4 p-6 lg:grid-cols-4 md:grid-cols-2">
          <section v-for="col in columns" :key="col.key" class="min-w-0 space-y-2">
            <h3 class="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wider" :class="col.tone">
              {{ t(`board.lanes.${col.key}`) }}
              <span class="rounded-full bg-muted px-1.5 text-[10px] tabular-nums text-muted-foreground">
                {{ columnItems(col).length + (col.key === 'running' ? data.queued.length : 0) }}
              </span>
            </h3>
            <BatchBoardCard
              v-for="{ item, lane } in columnItems(col)"
              :key="item.runId"
              :item="item"
              :lane="lane"
              :busy="busy.has(item.runId)"
              @open="openRun(item)"
              @message="message(item)"
              @stop="stop(item)"
              @commit="commit(item)"
              @push="push(item)"
              @finish="finish(item)"
              @discard="discard(item)"
            />
            <!-- Todos no worker picked up yet. -->
            <template v-if="col.key === 'running'">
              <div
                v-for="q in data.queued"
                :key="q.todo.id"
                class="rounded-lg border border-dashed border-border px-3 py-2 space-y-1"
              >
                <div class="flex items-center gap-1.5">
                  <Badge v-if="q.projectName" size="xs" tone="neutral">{{ q.projectName }}</Badge>
                  <span class="text-[10px] text-muted-foreground">{{ q.todo.done ? t('board.queuedDone') : t('board.queued') }}</span>
                </div>
                <p class="text-xs line-clamp-2">{{ q.todo.text.split('\n')[0] }}</p>
              </div>
            </template>
            <p
              v-if="!columnItems(col).length && !(col.key === 'running' && data.queued.length)"
              class="rounded-lg border border-dashed border-border/60 px-3 py-4 text-center text-[11px] text-muted-foreground/70"
            >
              {{ t(`board.empty.${col.key}`) }}
            </p>
          </section>
        </div>
      </template>
    </div>
  </div>
</template>
