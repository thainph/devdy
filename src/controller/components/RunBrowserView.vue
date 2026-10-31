<script setup lang="ts">
/**
 * Run browser — the home screen once a device is paired.
 *
 * A paired device may drive every run on the Host (SRS BR-005 v1.1: the trust
 * boundary is device approval, not per-run share), which on a busy Host means up
 * to RUN_LIST_MAX runs. A flat list of that many rows is unusable on a phone, so
 * this drills down: projects first, then that project's runs.
 *
 * Runs awaiting a decision live in the Inbox tab (every prompt, answerable in
 * place); here they only keep a badge and sort first. **Search** escapes the
 * drill-down: it matches across every project at once, because the run you want
 * is often easier to name than to locate.
 *
 * Backend-agnostic like the other controller views: state in via props, intent
 * out via events. The selected project lives in the PARENT so it survives
 * opening a run and coming back.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, ChevronLeft, ChevronRight, FolderGit2, Plus, RefreshCw, X } from 'lucide-vue-next'
import ConnectionBadge from './ConnectionBadge.vue'
import ControllerHeader from './ControllerHeader.vue'
import HeaderButton from './HeaderButton.vue'
import SearchField from './SearchField.vue'
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
  /** Start a new session in this project. */
  'new-session': [projectId: string]
  refresh: []
  disconnect: []
  'update:selectedProjectId': [projectId: string | null]
}>()

const { t } = useI18n()

const isActive = computed(() => props.status === 'connected')
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
  <div class="flex h-full min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="t('controller.browser.title')">
      <template #badge>
        <ConnectionBadge :status="status" :live="live" :status-label="statusLabel" :host-fingerprint="hostFingerprint" />
      </template>
      <HeaderButton :label="t('controller.browser.refresh')" @click="emit('refresh')">
        <RefreshCw class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
      <HeaderButton :label="t('controller.session.disconnect')" @click="emit('disconnect')">
        <X class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
    </ControllerHeader>

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
      <button
        v-if="selectedProject && !searching"
        class="ml-auto inline-flex items-center gap-1 h-6 shrink-0 rounded-md px-2 text-xs text-foreground/70 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed"
        :disabled="!isActive"
        @click="emit('new-session', selectedProject.id)"
      >
        <Plus class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('controller.browser.newSession') }}
      </button>
    </div>

    <!-- Search spans every project, so it is available at both levels. -->
    <SearchField
      v-model="query"
      :placeholder="t('controller.browser.searchPlaceholder')"
      :clear-label="t('controller.browser.clearSearch')"
    />

    <main class="flex-1 min-h-0 overflow-auto">
      <!-- ── Level 1: projects ────────────────────────────────────────────── -->
      <template v-if="showingProjects">
        <p v-if="!projectRows.length" class="px-3 py-8 text-center text-sm text-foreground/50">
          {{ isActive ? t('controller.browser.empty') : t('controller.browser.loading') }}
        </p>

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
        <p v-if="pendingRunIds.length" class="px-3 py-3 text-center text-[11px] text-foreground/45">
          {{ t('controller.browser.pendingInInbox') }}
        </p>
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
