<script setup lang="ts">
/**
 * Run browser — the home screen once a device is paired.
 *
 * A paired device may drive every run on the Host (SRS BR-005 v1.1: the trust
 * boundary is device approval, not per-run share), which on a busy Host means up
 * to RUN_LIST_MAX runs. A flat list of that many rows is unusable on a phone, so
 * this drills down: projects first, then that project's runs.
 *
 * Order is the desktop's, exactly: the Host sends projects in the sidebar order
 * and runs pinned-first, then by last activity, and nothing here re-sorts.
 * Runs awaiting a decision live in the Inbox tab (every prompt, answerable in
 * place); here they only keep a badge. **Search** escapes the drill-down: it
 * matches across every project at once, because the run you want is often
 * easier to name than to locate.
 *
 * Backend-agnostic like the other controller views: state in via props, intent
 * out via events. The selected project lives in the PARENT (the tab header's
 * project dropdown drives it) so it survives opening a run and coming back.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, ChevronRight, FolderGit2, Pin } from 'lucide-vue-next'
import FabButton from './FabButton.vue'
import SearchField from './SearchField.vue'
import type { ConnStatus } from '../connection'
import type { ProjectInfo, RunInfo } from '../protocol'
import { browserProjectRows } from '@/lib/controllerRunBrowser'

const props = defineProps<{
  status: ConnStatus
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

/** One row per project, in the desktop sidebar's order (projects with no
 * session yet included). */
const projectRows = computed(() =>
  browserProjectRows(props.projects, props.runs, pending.value, t('controller.browser.ungrouped')),
)

/** The project picked in the header — also one with no session yet, so a
 * first session can be started there. */
const selectedProject = computed(() =>
  props.selectedProjectId ? projectRows.value.find((p) => p.id === props.selectedProjectId) : undefined,
)

/** What the run list shows: search hits across everything, else the open
 * project's runs. */
const visibleRuns = computed(() => {
  if (searching.value) {
    const q = query.value.trim().toLowerCase()
    return props.runs.filter(
      (r) =>
        runTitle(r).toLowerCase().includes(q) ||
        projectName(r).toLowerCase().includes(q) ||
        r.id.toLowerCase().includes(q),
    )
  }
  if (!props.selectedProjectId) return []
  return props.runs.filter((r) => r.project_id === props.selectedProjectId)
})

/** Project list is the view only when nothing is selected and nothing is typed. */
const showingProjects = computed(() => !searching.value && !props.selectedProjectId)

const RUNNING_STATUSES = new Set(['running', 'streaming'])
const FAILED_STATUSES = new Set(['failed', 'error', 'cancelled'])

function statusDotClass(run: RunInfo): string {
  if (RUNNING_STATUSES.has(run.status)) return 'bg-success animate-pulse'
  if (FAILED_STATUSES.has(run.status)) return 'bg-destructive'
  return 'bg-foreground/25'
}

</script>

<template>
  <div class="relative flex h-full min-h-0 flex-col text-foreground">
    <!-- Search spans every project, so it is available at both levels. -->
    <SearchField
      v-model="query"
      :placeholder="t('controller.browser.searchPlaceholder')"
      :clear-label="t('controller.browser.clearSearch')"
    />

    <main class="flex-1 min-h-0 overflow-auto" :class="selectedProject && !searching ? 'pb-24' : ''">
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
            class="shrink-0 inline-flex items-center gap-1 rounded-md bg-warning/15 px-1.5 py-0.5 text-[10px] font-medium text-warning"
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
            class="shrink-0 inline-flex items-center gap-1 rounded-md bg-warning/15 px-1.5 py-0.5 text-[10px] font-medium text-warning"
            :title="t('controller.browser.needsApproval')"
          >
            <AlertTriangle class="h-3 w-3" :stroke-width="2" />
          </span>
          <Pin
            v-if="run.pinned"
            class="shrink-0 h-3.5 w-3.5 text-foreground/40"
            :stroke-width="2"
            :aria-label="t('controller.browser.pinned')"
          />
          <ChevronRight class="shrink-0 h-4 w-4 text-foreground/30" :stroke-width="2" />
        </button>
      </template>
    </main>

    <FabButton
      v-if="selectedProject && !searching"
      :label="t('controller.browser.newSession')"
      :disabled="!isActive"
      @click="emit('new-session', selectedProject.id)"
    />
  </div>
</template>
