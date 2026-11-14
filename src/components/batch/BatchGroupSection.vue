<script setup lang="ts">
// One batch on the Batch Board: a project's Todo Conductor (status, what it said
// last, a prompt waiting on it, "Open session" to chat with it) above its
// workers sorted into lanes. The solo pseudo batch has no conductor. Card
// actions bubble up to the board, which runs them.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Bot, ExternalLink, Folder, Rocket, Square } from 'lucide-vue-next'
import { Badge, Button, Card, StatusBadge } from '@/components/ui'
import BatchBoardCard from '@/components/batch/BatchBoardCard.vue'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useMarkdown } from '@/lib/markdown'
import {
  SOLO_BATCH,
  groupByLane,
  lastActivity,
  type BoardGroup,
  type BoardItem,
  type Lane,
  type LiveState,
} from '@/lib/batchBoard'

const props = defineProps<{
  group: BoardGroup
  /** Run ids with an action in flight. */
  busy: Set<string>
  /** Scrolled to and outlined (opened from a link). */
  highlighted?: boolean
}>()

const emit = defineEmits<{
  open: [item: { runId: string; projectId: string }]
  stop: [item: BoardItem]
  commit: [item: BoardItem]
  push: [item: BoardItem]
  finish: [item: BoardItem]
  discard: [item: BoardItem]
}>()

const { t } = useI18n()
const live = useLiveRunsStore()
const { renderText, loadMarkdown } = useMarkdown()
void loadMarkdown()

const solo = computed(() => props.group.batch.runId === SOLO_BATCH)
const conductor = computed(() => props.group.items.find((i) => i.role === 'conductor') ?? null)
const conductorLive = computed(() => (conductor.value ? live.get(conductor.value.runId) : undefined))
const conductorStatus = computed(() => conductorLive.value?.status || conductor.value?.status || '')
const conductorActivity = computed(() => lastActivity(conductorLive.value?.entries ?? [], 300))
const conductorPending = computed(() => conductorLive.value?.permissionQueue[0] ?? null)
const projectName = computed(() => conductor.value?.projectName || props.group.batch.projectName)
/** The batch title without its "🗂 project ·" lead (the project has its own badge). */
const title = computed(() => {
  const raw = (props.group.batch.title ?? '').replace(/^🗂\s*/, '')
  const lead = `${projectName.value} · `
  return raw.startsWith(lead) ? raw.slice(lead.length) : raw
})
const workers = computed(() => props.group.items.filter((i) => i.role !== 'conductor'))
const waiting = computed(() => props.group.queued.filter((q) => !q.todo.done))

function liveOf(runId: string): LiveState | undefined {
  const s = live.get(runId)
  if (!s) return undefined
  return { pending: s.permissionQueue.length, attention: s.attention.length, status: s.status || undefined }
}
const lanes = computed(() => groupByLane(props.group.items, liveOf))

/** Columns: what needs the user first. */
const COLUMNS: { key: string; lanes: Lane[]; tone: string }[] = [
  { key: 'needs', lanes: ['needs', 'alert'], tone: 'text-primary' },
  { key: 'running', lanes: ['running'], tone: 'text-info' },
  { key: 'review', lanes: ['review'], tone: 'text-success' },
  { key: 'closed', lanes: ['failed', 'closed'], tone: 'text-muted-foreground' },
]
function columnItems(col: { lanes: Lane[] }) {
  return col.lanes.flatMap((l) => lanes.value[l].map((item) => ({ item, lane: l })))
}

// Same answer path as the permission windows.
function decide(decision: 'allow' | 'deny', remember: boolean) {
  const c = conductor.value
  const req = conductorPending.value
  if (!c || !req) return
  if (remember && decision === 'allow') live.rememberAllowedTool(c.runId, req.tool_name, req.request_id)
  else if (remember && decision === 'deny') live.rememberDeniedTool(c.runId, req.tool_name, req.request_id)
  void live.respondPermission(c.runId, req, decision, { remember })
}
function answer(answers: Record<string, string>) {
  const c = conductor.value
  const req = conductorPending.value
  if (c && req) void live.respondPermission(c.runId, req, 'allow', { answers })
}
</script>

<template>
  <Card body-class="p-0" :class="highlighted ? 'ring-2 ring-primary/60' : ''">
    <!-- The conductor (or the solo heading). -->
    <div class="space-y-2 border-b border-border/60 px-4 py-3">
      <div class="flex flex-wrap items-center gap-2 min-w-0">
        <template v-if="solo">
          <Rocket class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
          <span class="text-[13px] font-semibold">{{ t('board.solo') }}</span>
          <span class="text-[11px] text-muted-foreground">{{ t('board.soloHint', { n: group.batch.workerCount }) }}</span>
        </template>
        <template v-else>
          <Badge size="sm" tone="neutral" class="shrink-0">
            <Folder class="h-3 w-3" :stroke-width="1.75" />{{ projectName }}
          </Badge>
          <Bot class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
          <span class="min-w-0 truncate text-[13px] font-medium" :title="group.batch.title ?? ''">{{ title || t('board.conductor') }}</span>
          <StatusBadge v-if="conductorStatus" :status="conductorStatus" size="xs" run-type="session" />
          <span class="text-[11px] text-muted-foreground">
            {{ t('board.summary', { workers: workers.length, queued: waiting.length }) }}
          </span>
          <div class="ml-auto flex shrink-0 items-center gap-1">
            <Button
              v-if="conductor && conductorStatus === 'running'"
              variant="destructive-ghost"
              size="xs"
              :disabled="busy.has(conductor.runId)"
              :title="t('board.stopConductorTitle')"
              @click="emit('stop', conductor)"
            >
              <Square class="h-3 w-3" :stroke-width="2" />{{ t('board.stop') }}
            </Button>
            <Button v-if="conductor" variant="outline" size="xs" :title="t('board.openConductorTitle')" @click="emit('open', conductor)">
              <ExternalLink class="h-3 w-3" :stroke-width="2" />{{ t('board.openConductor') }}
            </Button>
          </div>
        </template>
      </div>
      <p v-if="conductorActivity" class="text-xs text-muted-foreground line-clamp-3 whitespace-pre-line">{{ conductorActivity }}</p>
      <div v-if="conductorPending" class="-mx-4 border-y border-border/60 bg-muted/30">
        <PermissionPrompt
          :key="conductorPending.request_id"
          :request="conductorPending"
          :allowed-tools="conductorLive?.allowedTools ?? []"
          :render-text="renderText"
          @decide="decide"
          @answer="answer"
        />
      </div>
      <!-- Todos no worker picked up yet. -->
      <div v-if="waiting.length" class="flex flex-wrap items-center gap-1.5">
        <span class="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.queued') }}</span>
        <span
          v-for="q in waiting"
          :key="q.todo.id"
          class="max-w-[16rem] truncate rounded border border-dashed border-border px-1.5 py-0.5 text-[11px]"
          :title="q.todo.text"
        >
          {{ q.todo.text.split('\n')[0] }}
        </span>
      </div>
    </div>

    <!-- Workers by lane. -->
    <div class="grid gap-4 p-4 lg:grid-cols-4 md:grid-cols-2">
      <section v-for="col in COLUMNS" :key="col.key" class="min-w-0 space-y-2">
        <h3 class="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wider" :class="col.tone">
          {{ t(`board.lanes.${col.key}`) }}
          <span class="rounded-full bg-muted px-1.5 text-[10px] tabular-nums text-muted-foreground">{{ columnItems(col).length }}</span>
        </h3>
        <BatchBoardCard
          v-for="{ item, lane } in columnItems(col)"
          :key="item.runId"
          :item="item"
          :lane="lane"
          :busy="busy.has(item.runId)"
          :show-project="solo"
          @open="emit('open', item)"
          @stop="emit('stop', item)"
          @commit="emit('commit', item)"
          @push="emit('push', item)"
          @finish="emit('finish', item)"
          @discard="emit('discard', item)"
        />
        <p
          v-if="!columnItems(col).length"
          class="rounded-lg border border-dashed border-border/60 px-3 py-3 text-center text-[11px] text-muted-foreground/70"
        >
          {{ t(`board.empty.${col.key}`) }}
        </p>
      </section>
    </div>
  </Card>
</template>
