<script setup lang="ts">
/**
 * Run browser — the home screen once a device is paired.
 *
 * A paired device may drive every run on the Host (SRS BR-005 v1.1: the trust
 * boundary is device approval, not per-run share), which on a busy Host means up
 * to RUN_LIST_MAX runs. A flat list of that many rows is unusable on a phone, so
 * this drills down: projects first, then that project's runs.
 *
 * Two things deliberately escape the drill-down:
 *
 *  - **Runs awaiting a decision** are pinned to the top of the project list.
 *    They are the only thing actually blocking work on the Host, and burying one
 *    two taps deep inside a project would defeat the reason the Host forwards
 *    prompts for runs the user is not watching.
 *  - **Search** matches across every project at once, because the run you want
 *    is often easier to name than to locate.
 *
 * Backend-agnostic like the other controller views: state in via props, intent
 * out via events. The selected project lives in the PARENT so it survives
 * opening a run and coming back.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertTriangle, ChevronLeft, ChevronRight, FolderGit2, Loader2, RefreshCw,
  Search, ShieldCheck, Wifi, WifiOff, X,
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
  /** Project being browsed; null shows the project list. Owned by the parent. */
  selectedProjectId: string | null
}>()

const emit = defineEmits<{
  open: [runId: string]
  refresh: []
  disconnect: []
  'update:selectedProjectId': [projectId: string | null]
}>()

const { t } = useI18n()

const isActive = computed(() => props.status === 'connected')
const shortFp = computed(() => (props.hostFingerprint ? props.hostFingerprint.slice(0, 12) : ''))
const pending = computed(() => new Set(props.pendingRunIds))

const query = ref('')
const searching = computed(() => query.value.trim().length > 0)

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

/** Pending-first ordering. `runs` already arrives newest-activity-first from the
 * Host and Array.prototype.sort is stable, so that order survives within a tier. */
function pendingFirst(items: RunInfo[]): RunInfo[] {
  return [...items].sort(
    (a, b) => Number(pending.value.has(b.id)) - Number(pending.value.has(a.id)),
  )
}

/** One row per project, ordered by most recent activity (runs arrive sorted, so
 * first appearance wins). Projects with nothing waiting sort after those that
 * have something waiting. */
const projectRows = computed(() => {
  const byId = new Map<string, { id: string; name: string; total: number; pending: number }>()
  for (const run of props.runs) {
    const id = run.project_id
    let row = byId.get(id)
    if (!row) {
      row = { id, name: projectName(run), total: 0, pending: 0 }
      byId.set(id, row)
    }
    row.total += 1
    if (pending.value.has(run.id)) row.pending += 1
  }
  return [...byId.values()].sort((a, b) => Number(b.pending > 0) - Number(a.pending > 0))
})

/** Runs awaiting a decision, across every project — pinned above the projects. */
const waitingRuns = computed(() => props.runs.filter((r) => pending.value.has(r.id)))

const selectedProject = computed(() =>
  props.selectedProjectId ? projectRows.value.find((p) => p.id === props.selectedProjectId) : undefined,
)

/** What the run list shows: search hits across everything, else the open
 * project's runs. */
const visibleRuns = computed(() => {
  if (searching.value) {
    const q = query.value.trim().toLowerCase()
    return pendingFirst(
      props.runs.filter(
        (r) =>
          runTitle(r).toLowerCase().includes(q) ||
          projectName(r).toLowerCase().includes(q) ||
          r.id.toLowerCase().includes(q),
      ),
    )
  }
  if (!props.selectedProjectId) return []
  return pendingFirst(props.runs.filter((r) => r.project_id === props.selectedProjectId))
})

/** Project list is the view only when nothing is selected and nothing is typed. */
const showingProjects = computed(() => !searching.value && !props.selectedProjectId)

const RUNNING_STATUSES = new Set(['running', 'streaming'])
const FAILED_STATUSES = new Set(['failed', 'error', 'cancelled'])

function statusDotClass(run: RunInfo): string {
  if (RUNNING_STATUSES.has(run.status)) return 'bg-emerald-500 animate-pulse'
  if (FAILED_STATUSES.has(run.status)) return 'bg-destructive'
  return 'bg-foreground/25'
}

/** Leaving a project also clears a search, so Back always means "one level up"
 * rather than sometimes landing on a filtered list the user forgot about. */
function backToProjects(): void {
  query.value = ''
  emit('update:selectedProjectId', null)
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

    <!-- Title row: doubles as the level-up control once inside a project. -->
    <div class="shrink-0 flex items-center gap-1 px-3 py-1.5 border-b border-border/60">
      <button
        v-if="selectedProject && !searching"
        class="inline-flex items-center justify-center h-6 w-6 -ml-1 shrink-0 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
        :title="t('controller.browser.backToProjects')"
        @click="backToProjects"
      >
        <ChevronLeft class="h-4 w-4" :stroke-width="2" />
      </button>
      <p class="text-xs font-medium truncate">
        {{
          searching
            ? t('controller.browser.search')
            : selectedProject
              ? selectedProject.name
              : t('controller.browser.projectsTitle')
        }}
      </p>
    </div>

    <!-- Search spans every project, so it is available at both levels. -->
    <div class="shrink-0 px-3 py-2 border-b border-border/40">
      <div class="relative">
        <Search
          class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-foreground/40"
          :stroke-width="2"
        />
        <input
          v-model="query"
          type="search"
          inputmode="search"
          autocomplete="off"
          :placeholder="t('controller.browser.searchPlaceholder')"
          class="search-input w-full rounded-md border border-border bg-background py-1.5 pl-7 pr-7 text-sm outline-none placeholder:text-foreground/40 focus:border-foreground/30"
        />
        <button
          v-if="searching"
          class="absolute right-1.5 top-1/2 inline-flex h-5 w-5 -translate-y-1/2 items-center justify-center rounded text-foreground/40 hover:text-foreground cursor-pointer"
          :title="t('controller.browser.clearSearch')"
          @click="query = ''"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </div>
    </div>

    <main class="flex-1 min-h-0 overflow-auto">
      <!-- ── Level 1: projects ────────────────────────────────────────────── -->
      <template v-if="showingProjects">
        <p v-if="!projectRows.length" class="px-3 py-8 text-center text-sm text-foreground/50">
          {{ isActive ? t('controller.browser.empty') : t('controller.browser.loading') }}
        </p>

        <!-- Anything blocked on a decision jumps the hierarchy. -->
        <section v-if="waitingRuns.length">
          <h2
            class="px-3 py-1.5 bg-amber-500/10 text-[10px] font-medium uppercase tracking-wider text-amber-600 dark:text-amber-400 border-b border-border/40"
          >
            {{ t('controller.browser.needsApprovalSection') }}
          </h2>
          <button
            v-for="run in waitingRuns"
            :key="'w-' + run.id"
            class="w-full flex items-center gap-2.5 px-3 py-2.5 text-left border-b border-border/40 hover:bg-accent transition-colors cursor-pointer"
            :title="t('controller.browser.openRun')"
            @click="emit('open', run.id)"
          >
            <AlertTriangle class="shrink-0 h-4 w-4 text-amber-500" :stroke-width="2" />
            <span class="min-w-0 flex-1">
              <span class="block text-sm truncate">{{ runTitle(run) }}</span>
              <span class="block text-[10px] text-foreground/45 truncate">{{ projectName(run) }}</span>
            </span>
            <ChevronRight class="shrink-0 h-4 w-4 text-foreground/30" :stroke-width="2" />
          </button>
        </section>

        <button
          v-for="p in projectRows"
          :key="p.id"
          class="w-full flex items-center gap-2.5 px-3 py-3 text-left border-b border-border/40 hover:bg-accent transition-colors cursor-pointer"
          @click="emit('update:selectedProjectId', p.id)"
        >
          <FolderGit2 class="shrink-0 h-4 w-4 text-foreground/40" :stroke-width="2" />
          <span class="min-w-0 flex-1">
            <span class="block text-sm truncate">{{ p.name }}</span>
            <span class="block text-[10px] text-foreground/45">
              {{ t('controller.browser.runCount', p.total) }}
            </span>
          </span>
          <span
            v-if="p.pending"
            class="shrink-0 inline-flex items-center gap-1 rounded-md bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400"
            :title="t('controller.browser.needsApproval')"
          >
            <AlertTriangle class="h-3 w-3" :stroke-width="2" /> {{ p.pending }}
          </span>
          <ChevronRight class="shrink-0 h-4 w-4 text-foreground/30" :stroke-width="2" />
        </button>
      </template>

      <!-- ── Level 2 (or search results): runs ────────────────────────────── -->
      <template v-else>
        <p v-if="!visibleRuns.length" class="px-3 py-8 text-center text-sm text-foreground/50">
          {{ searching ? t('controller.browser.noResults') : t('controller.browser.noRuns') }}
        </p>
        <button
          v-for="run in visibleRuns"
          :key="run.id"
          class="w-full flex items-center gap-2.5 px-3 py-2.5 text-left border-b border-border/40 hover:bg-accent transition-colors cursor-pointer"
          :title="t('controller.browser.openRun')"
          @click="emit('open', run.id)"
        >
          <span class="shrink-0 h-2 w-2 rounded-full" :class="statusDotClass(run)" />
          <span class="min-w-0 flex-1">
            <span class="block text-sm truncate">{{ runTitle(run) }}</span>
            <span class="block text-[10px] text-foreground/45 truncate">
              <!-- While searching the project matters more than the engine: the
                   results span projects, so it is the disambiguator. -->
              {{ searching ? projectName(run) : run.engine || run.type }}
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
      </template>
    </main>
  </div>
</template>

<style scoped>
/* `type="search"` gets a native clear button in WebKit/Blink, which would sit
   next to our own — two X's side by side. Ours stays: it matches the rest of the
   controller's iconography and is positioned with the input's padding. */
.search-input::-webkit-search-cancel-button,
.search-input::-webkit-search-decoration {
  -webkit-appearance: none;
  appearance: none;
}
</style>
