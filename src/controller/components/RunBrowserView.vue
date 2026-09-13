<script setup lang="ts">
/**
 * Run browser — the home screen once a device is paired.
 *
 * A paired device may drive every run on the Host (SRS BR-005 v1.1: the trust
 * boundary is device approval, not per-run share), so this lists them grouped by
 * project and hands the chosen one to the parent, which opens it.
 *
 * Runs awaiting a permission decision float to the top of their group and carry
 * a badge: that is the one thing actually blocking work on the Host, and the
 * reason the Host forwards prompts for runs the user is not watching.
 *
 * Backend-agnostic like the other controller views: state in via props, intent
 * out via events.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertTriangle, ChevronRight, Loader2, RefreshCw, ShieldCheck, Wifi, WifiOff, X,
} from 'lucide-vue-next'
import type { ConnStatus } from '../connection'
import type { ProjectInfo, RunInfo } from '../protocol'

const props = defineProps<{
  status: ConnStatus
  live: boolean
  hostFingerprint: string | null
  statusLabel: string
  runs: RunInfo[]
  projects: ProjectInfo[]
  /** Run ids awaiting a permission decision. */
  pendingRunIds: string[]
}>()

const emit = defineEmits<{
  open: [runId: string]
  refresh: []
  disconnect: []
}>()

const { t } = useI18n()

const isActive = computed(() => props.status === 'connected')
const shortFp = computed(() => (props.hostFingerprint ? props.hostFingerprint.slice(0, 12) : ''))
const pending = computed(() => new Set(props.pendingRunIds))

/** Project name for a run: prefer the run's own denormalized name, fall back to
 * the project list, then to a neutral bucket so nothing is ever dropped. */
function projectName(run: RunInfo): string {
  const own = run.project_name?.trim()
  if (own) return own
  const p = props.projects.find((x) => x.id === run.project_id)
  return p?.name?.trim() || t('controller.browser.ungrouped')
}

function runTitle(run: RunInfo): string {
  const title = run.title?.trim()
  return title ? title : t('controller.browser.untitled', { id: run.id.slice(0, 8) })
}

/** Runs bucketed by project, each bucket ordered with anything awaiting a
 * decision first. `runs` already arrives newest-activity-first from the Host, and
 * Array.prototype.sort is stable, so that order survives within each tier. */
const groups = computed(() => {
  const byProject = new Map<string, RunInfo[]>()
  for (const run of props.runs) {
    const name = projectName(run)
    const bucket = byProject.get(name)
    if (bucket) bucket.push(run)
    else byProject.set(name, [run])
  }
  return [...byProject.entries()].map(([name, items]) => ({
    name,
    runs: [...items].sort(
      (a, b) => Number(pending.value.has(b.id)) - Number(pending.value.has(a.id)),
    ),
  }))
})

const RUNNING_STATUSES = new Set(['running', 'streaming'])
const FAILED_STATUSES = new Set(['failed', 'error', 'cancelled'])

function statusDotClass(run: RunInfo): string {
  if (RUNNING_STATUSES.has(run.status)) return 'bg-emerald-500 animate-pulse'
  if (FAILED_STATUSES.has(run.status)) return 'bg-destructive'
  return 'bg-foreground/25'
}
</script>

<template>
  <div class="flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <!-- Top bar: mirrors SessionView so the two screens read as one app. -->
    <header class="shrink-0 flex items-center gap-2 px-3 py-2 border-b border-border">
      <span
        class="inline-flex items-center gap-1.5 text-xs font-medium min-w-0"
        :class="isActive && live ? 'text-emerald-500' : status === 'error' ? 'text-destructive' : 'text-amber-500'"
      >
        <Loader2
          v-if="status === 'joining' || status === 'handshaking' || status === 'reconnecting'"
          class="h-3.5 w-3.5 animate-spin"
          :stroke-width="2"
        />
        <Wifi v-else-if="isActive" class="h-3.5 w-3.5" :stroke-width="2" />
        <WifiOff v-else class="h-3.5 w-3.5" :stroke-width="2" />
        <span class="truncate">{{ statusLabel }}</span>
      </span>
      <span
        v-if="shortFp"
        class="inline-flex items-center gap-1 text-[10px] font-mono text-foreground/45"
        :title="t('controller.session.verifiedFingerprint', { fp: hostFingerprint })"
      >
        <ShieldCheck class="h-3 w-3 text-emerald-500" :stroke-width="2" /> {{ shortFp }}…
      </span>
      <div class="ml-auto flex items-center gap-1.5">
        <button
          class="inline-flex items-center justify-center h-7 w-7 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
          :title="t('controller.browser.refresh')"
          @click="emit('refresh')"
        >
          <RefreshCw class="h-4 w-4" :stroke-width="2" />
        </button>
        <button
          class="inline-flex items-center justify-center h-7 w-7 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
          :title="t('controller.session.disconnect')"
          @click="emit('disconnect')"
        >
          <X class="h-4 w-4" :stroke-width="2" />
        </button>
      </div>
    </header>

    <div class="shrink-0 px-3 py-1.5 border-b border-border/60">
      <p class="text-xs font-medium">{{ t('controller.browser.title') }}</p>
    </div>

    <main class="flex-1 min-h-0 overflow-auto">
      <p v-if="!runs.length" class="px-3 py-8 text-center text-sm text-foreground/50">
        {{ isActive ? t('controller.browser.empty') : t('controller.browser.loading') }}
      </p>

      <section v-for="group in groups" :key="group.name">
        <h2
          class="sticky top-0 z-10 px-3 py-1.5 bg-background/95 backdrop-blur text-[10px] font-medium uppercase tracking-wider text-foreground/45 border-b border-border/40"
        >
          {{ group.name }}
        </h2>
        <button
          v-for="run in group.runs"
          :key="run.id"
          class="w-full flex items-center gap-2.5 px-3 py-2.5 text-left border-b border-border/40 hover:bg-accent transition-colors cursor-pointer"
          :title="t('controller.browser.openRun')"
          @click="emit('open', run.id)"
        >
          <span class="shrink-0 h-2 w-2 rounded-full" :class="statusDotClass(run)" />
          <span class="min-w-0 flex-1">
            <span class="block text-sm truncate">{{ runTitle(run) }}</span>
            <span class="block text-[10px] text-foreground/45 truncate">
              {{ run.engine || run.type }}
            </span>
          </span>
          <span
            v-if="pending.has(run.id)"
            class="shrink-0 inline-flex items-center gap-1 rounded-md bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400"
            :title="t('controller.browser.needsApproval')"
          >
            <AlertTriangle class="h-3 w-3" :stroke-width="2" />
          </span>
          <ChevronRight class="shrink-0 h-4 w-4 text-foreground/30" :stroke-width="2" />
        </button>
      </section>
    </main>
  </div>
</template>
