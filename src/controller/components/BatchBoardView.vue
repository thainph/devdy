<script setup lang="ts">
/**
 * The Batch Board on the phone (mirrors the desktop's BatchBoardView): a todo
 * batch's workers grouped by what they need from the user. Prompts themselves
 * are answered in the Inbox (every open prompt is there), so a "needs you" row
 * shows a count and opens the session; any row opens its session.
 */
import { computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, GitBranch, RefreshCw } from 'lucide-vue-next'
import { groupByLane, roleLabel, type BoardItem, type Lane } from '@/lib/batchBoard'
import ControllerHeader from './ControllerHeader.vue'
import HeaderButton from './HeaderButton.vue'
import type { BatchBoardKind, RunInfo } from '../protocol'

const props = defineProps<{
  batch: Omit<BatchBoardKind, 'kind'> | null
  failed: boolean
  runs: RunInfo[]
  /** run id → its open prompt (runs with none are absent). */
  pendingByRun: Record<string, unknown>
}>()

const emit = defineEmits<{
  back: []
  load: [batchRunId: string | null]
  open: [runId: string]
}>()

const { t } = useI18n()

onMounted(() => emit('load', props.batch?.batch_run_id ?? null))

const runStatus = computed(() => new Map(props.runs.map((r) => [r.id, r.status])))
const items = computed(() => props.batch?.board?.items ?? [])
const conductor = computed(() => items.value.find((i) => i.role === 'conductor') ?? null)
const lanes = computed(() =>
  groupByLane(items.value, (runId) => ({
    pending: props.pendingByRun[runId] ? 1 : 0,
    attention: 0,
    status: runStatus.value.get(runId),
  })),
)
const sections = computed(() =>
  (['needs', 'running', 'review', 'failed', 'closed'] as Lane[])
    .map((lane) => ({ lane, rows: lanes.value[lane] }))
    .filter((s) => s.rows.length),
)

function heading(item: BoardItem): string {
  const text = item.todo?.text ?? item.title?.replace(/^\[[^\]]*\]\s*/, '') ?? ''
  return text.split('\n')[0].trim() || t('board.untitled')
}

function pick(e: Event) {
  emit('load', (e.target as HTMLSelectElement).value || null)
}
</script>

<template>
  <div class="app-shell relative flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="t('board.title')">
      <template #leading>
        <HeaderButton class="-ml-1.5" :label="t('controller.data.back')" @click="emit('back')">
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </HeaderButton>
      </template>
      <HeaderButton :label="t('board.refresh')" @click="emit('load', batch?.batch_run_id ?? null)">
        <RefreshCw class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
    </ControllerHeader>

    <div class="flex-1 min-h-0 overflow-auto">
      <p v-if="failed" class="px-3 py-6 text-center text-sm text-destructive">{{ t('controller.data.loadFailed') }}</p>
      <p v-else-if="!batch" class="px-3 py-6 text-center text-sm text-muted-foreground">…</p>
      <div v-else-if="!batch.batches.length" class="px-4 py-10 text-center space-y-2">
        <p class="text-sm font-medium">{{ t('board.emptyTitle') }}</p>
        <p class="text-xs text-muted-foreground">{{ t('board.emptyBody') }}</p>
      </div>
      <template v-else>
        <div class="px-3 py-2 border-b border-border/40">
          <select
            class="ui-field h-9 w-full rounded-md border border-border bg-background px-2 text-sm"
            :value="batch.batch_run_id ?? ''"
            @change="pick"
          >
            <option v-for="b in batch.batches" :key="b.runId" :value="b.runId">
              {{ b.title || t('board.untitled') }} · {{ b.projectName }}
            </option>
          </select>
        </div>

        <button
          v-if="conductor"
          type="button"
          class="flex w-full items-center gap-2 px-3 py-2.5 border-b border-border/40 text-left active:bg-accent"
          @click="emit('open', conductor.runId)"
        >
          <span class="text-sm font-medium">{{ t('board.conductor') }}</span>
          <span class="text-xs text-muted-foreground">· {{ runStatus.get(conductor.runId) ?? conductor.status }}</span>
          <span class="ml-auto text-xs text-muted-foreground">
            {{ t('board.summary', { workers: items.length - 1, queued: batch.board?.queued.length ?? 0 }) }}
          </span>
        </button>

        <section v-for="s in sections" :key="s.lane">
          <h2 class="px-3 pt-3 pb-1 text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            {{ t(`board.mobileLanes.${s.lane}`) }} · {{ s.rows.length }}
          </h2>
          <button
            v-for="item in s.rows"
            :key="item.runId"
            type="button"
            class="block w-full px-3 py-2.5 border-b border-border/40 text-left active:bg-accent"
            @click="emit('open', item.runId)"
          >
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground">
              <span class="truncate">{{ item.projectName }}</span>
              <span v-if="roleLabel(item.title)">· {{ roleLabel(item.title) }}</span>
              <span v-if="s.lane === 'needs'" class="ml-auto shrink-0 rounded-full bg-primary/15 px-2 text-primary">
                {{ t('board.mobilePending') }}
              </span>
            </div>
            <p class="mt-0.5 text-sm line-clamp-2">{{ heading(item) }}</p>
            <p v-if="item.worktree" class="mt-0.5 flex items-center gap-1 font-mono text-[11px] text-muted-foreground">
              <GitBranch class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              <span class="truncate">{{ item.worktree.branch }}</span>
              <span v-if="item.diff" class="shrink-0">· +{{ item.diff.insertions }} −{{ item.diff.deletions }}</span>
            </p>
          </button>
        </section>

        <section v-if="batch.board?.queued.length">
          <h2 class="px-3 pt-3 pb-1 text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            {{ t('board.queued') }} · {{ batch.board.queued.length }}
          </h2>
          <div v-for="q in batch.board.queued" :key="q.todo.id" class="px-3 py-2.5 border-b border-border/40">
            <p class="text-xs text-muted-foreground">{{ q.projectName }}</p>
            <p class="text-sm line-clamp-2">{{ q.todo.text.split('\n')[0] }}</p>
          </div>
        </section>
      </template>
    </div>
  </div>
</template>
