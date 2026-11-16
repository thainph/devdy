<script setup lang="ts">
// Work Dashboard — running sessions across every project (`list_active_runs`),
// as a horizontal strip of cards: awaiting a permission first (amber, "Review"
// opens the session that holds the prompt — a worker folded into its
// conductor's card included; a menu picks one when several wait — where the
// full prompt is shown, never approved blind from here), then the longest
// running. Each card: name, project, engine, duration, status; actions: open, stop.
import { useI18n } from 'vue-i18n'
import { Activity, ChevronDown, ShieldAlert, Square, Users } from 'lucide-vue-next'
import { Banner, Button, DropdownItem, DropdownMenu } from '@/components/ui'
import type { DashboardRun, WaitingSession } from '@/lib/workDashboard'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import { useDurationLabel } from './useDurationLabel'

defineProps<{
  runs: DashboardRun[]
  state: SourceState
  now: number
  /** Run ids being stopped. */
  stopping: Set<string>
}>()

const emit = defineEmits<{ open: [projectId: string, runId: string]; stop: [row: DashboardRun] }>()

function openRow(row: DashboardRun) {
  emit('open', row.run.project_id, row.run.run_id)
}

function openWaiting(w: WaitingSession) {
  emit('open', w.projectId, w.runId)
}

function waitingLabel(w: WaitingSession): string {
  const name = w.title?.trim() || t('dashboard.runs.untitled')
  return t('dashboard.runs.waitingItem', { name, n: w.pending }, w.pending)
}

const { t } = useI18n()
const { duration } = useDurationLabel()

function label(row: DashboardRun): string {
  const r = row.run
  if (r.title?.trim()) return r.title.trim()
  if (r.run_type !== 'session' && r.ref_number != null) {
    return `${r.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${r.ref_number}`
  }
  return t('dashboard.runs.untitled')
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.runs.title')"
    :icon="Activity"
    :count="runs.length"
    :count-title="t('dashboard.runs.countTitle')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :stale-after="Infinity"
  >
    <Banner v-if="state.error" tone="destructive" class="m-3">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <p v-if="!state.error && runs.length === 0" class="px-4 py-4 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.runs.empty') }}
    </p>
    <div v-else class="flex gap-2 overflow-x-auto p-3">
      <div
        v-for="row in runs"
        :key="row.run.run_id"
        class="flex w-60 shrink-0 flex-col gap-1 rounded-md border px-2.5 py-2"
        :class="row.pending > 0 ? 'border-warning/40 bg-warning/10' : 'border-border/60 bg-muted/20'"
      >
        <button type="button" class="min-w-0 cursor-pointer text-left" @click="openRow(row)">
          <p class="truncate text-xs font-medium text-foreground" :title="label(row)">{{ label(row) }}</p>
          <p class="truncate text-[11px] text-muted-foreground">
            {{ row.run.project_name }}<template v-if="row.run.engine"> · {{ row.run.engine }}</template><template v-if="row.run.model"> · {{ row.run.model }}</template>
          </p>
        </button>
        <div class="flex items-center gap-1.5 text-[11px]">
          <span v-if="row.pending > 0" class="inline-flex items-center gap-1 font-medium text-warning">
            <ShieldAlert class="h-3 w-3" :stroke-width="2" />
            {{ t('dashboard.runs.waitingPermission', { n: row.pending }, row.pending) }}
          </span>
          <span v-else-if="row.run.status !== 'running'" class="text-muted-foreground">{{ t('dashboard.runs.idle') }}</span>
          <span v-else class="inline-flex items-center gap-1 text-primary">
            <span class="relative flex h-1.5 w-1.5">
              <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-primary opacity-60" />
              <span class="relative inline-flex h-1.5 w-1.5 rounded-full bg-primary" />
            </span>
            {{ t('dashboard.runs.running') }}
          </span>
          <span v-if="row.sinceMs !== null" class="text-muted-foreground">· {{ duration(row.sinceMs) }}</span>
          <span v-if="row.workerCount" class="inline-flex items-center gap-0.5 text-muted-foreground" :title="t('dashboard.runs.workers', { n: row.workerCount }, row.workerCount)">
            · <Users class="h-3 w-3" :stroke-width="1.75" />{{ row.workerCount }}
          </span>
        </div>
        <div class="mt-0.5 flex items-center gap-1">
          <DropdownMenu v-if="row.waiting.length > 1" align="left">
            <template #trigger>
              <Button variant="primary" size="xs">
                {{ t('dashboard.runs.review') }}
                <ChevronDown class="h-3 w-3" :stroke-width="2" />
              </Button>
            </template>
            <DropdownItem v-for="w in row.waiting" :key="w.runId" @click="openWaiting(w)">
              <ShieldAlert class="h-3.5 w-3.5 text-warning" :stroke-width="1.75" />
              {{ waitingLabel(w) }}
              <span v-if="w.worker" class="text-[11px] text-muted-foreground">{{ t('dashboard.runs.worker') }}</span>
            </DropdownItem>
          </DropdownMenu>
          <Button v-else-if="row.waiting.length === 1" variant="primary" size="xs" @click="openWaiting(row.waiting[0])">
            {{ t('dashboard.runs.review') }}
          </Button>
          <Button v-else variant="outline" size="xs" @click="openRow(row)">
            {{ t('dashboard.runs.open') }}
          </Button>
          <Button
            variant="destructive-ghost"
            size="xs"
            :disabled="stopping.has(row.run.run_id)"
            @click="emit('stop', row)"
          >
            <Square class="h-3 w-3" :stroke-width="2" />{{ t('dashboard.runs.stop') }}
          </Button>
        </div>
      </div>
    </div>
  </DashboardWidget>
</template>
