<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'
import { Columns2, ExternalLink, FolderOpen, X } from 'lucide-vue-next'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useProjectsStore } from '@/stores/projects'
import { useRunsStore } from '@/stores/runs'
import { useDragSort } from '@/composables/useDragSort'
import { SURFACE_META } from '@/lib/surfaceMeta'
import { surfaceForPath, surfaceTabId, cleanParams } from '@/lib/surfaces'
import { goToEntry, popOutSurface } from '@/lib/surfaceOpen'

/**
 * The shared tab strip of the main window: one tab per open PROJECT (its runs
 * are switched from RunView's History list; the tab returns to the last-viewed
 * run) and one per open SURFACE — PR Inbox, Calendar, Slack threads, Todos, a
 * file… shown in this window instead of their own (lib/surfaces). A surface tab
 * can pop out into its own window or go to the right half of a split.
 * Aggregate live status (any run streaming / awaiting permission) is read from
 * liveRuns.
 */

const { t } = useI18n()
const route = useRoute()
const tabsStore = useWorkspaceTabsStore()
const live = useLiveRunsStore()
const projectsStore = useProjectsStore()
const runsStore = useRunsStore()

const activeProjectId = computed(() =>
  typeof route.params.projectId === 'string' ? route.params.projectId : null,
)

/** The surface tab the router is showing (the left / only pane), if any. */
const routedSurfaceId = computed(() => {
  const kind = surfaceForPath(route.path)
  return kind ? surfaceTabId(kind, cleanParams(kind, route.query)) : null
})

function isActive(e: TabEntry): boolean {
  if (e.type === 'project') return e.projectId === activeProjectId.value && tabsStore.focusedPane === 'left'
  if (e.id === tabsStore.splitId) return tabsStore.focusedPane === 'right'
  return e.id === routedSurfaceId.value && tabsStore.focusedPane === 'left'
}

/** Shown in the half that doesn't have the focus — dimmer than active, still marked. */
function isVisible(e: TabEntry): boolean {
  if (e.type === 'project') return e.projectId === activeProjectId.value
  return e.id === tabsStore.splitId || e.id === routedSurfaceId.value
}

function projectName(projectId: string): string {
  return projectsStore.projects.find((p) => p.id === projectId)?.name ?? t('misc.tabs.project')
}

function surfaceLabel(e: Extract<TabEntry, { type: 'surface' }>): string {
  if (e.surface === 'file' && e.params.path) return e.params.path.split('/').pop() || e.params.path
  return t(SURFACE_META[e.surface].labelKey)
}

interface ProjectState {
  running: boolean
  pending: number
}

/**
 * Live status per open project, built in one pass over the sessions. A computed
 * rather than a per-tab helper so a drag — which re-renders the strip on every
 * pointer frame — doesn't re-scan every session four times per tab per frame.
 */
const states = computed(() => {
  const map: Record<string, ProjectState> = {}
  for (const tab of tabsStore.tabs) map[tab.projectId] = { running: false, pending: 0 }
  live.sessions.forEach((s) => {
    // A worker is shown (and its permissions answered) inside its conductor's
    // session, which may be in another project — light that project's tab.
    const parentId = runsStore.runMeta.get(s.runId)?.conductor_run_id
    const owner = (parentId && runsStore.runMeta.get(parentId)?.project_id) || s.projectId
    const st = map[owner]
    if (!st) return
    if (s.status === 'running') st.running = true
    st.pending += s.permissionQueue.length
  })
  return map
})

function stateOf(projectId: string): ProjectState {
  return states.value[projectId] ?? { running: false, pending: 0 }
}

function select(e: TabEntry) {
  if (e.type === 'surface' && e.id === tabsStore.splitId) {
    tabsStore.focusedPane = 'right'
    return
  }
  tabsStore.focusedPane = 'left'
  if (isVisible(e)) return
  if (e.type === 'project') {
    // A remembered run from another project (pinned before cross-project workers
    // were filed correctly) would open that project's session under this tab.
    const lastOwner = e.lastRunId ? runsStore.runMeta.get(e.lastRunId)?.project_id : undefined
    void goToEntry(e.lastRunId && lastOwner && lastOwner !== e.projectId ? { ...e, lastRunId: null } : e)
    return
  }
  void goToEntry(e)
}

function closeTab(e: TabEntry, ev?: MouseEvent) {
  ev?.stopPropagation()
  const wasRouted = e.type === 'project' ? e.projectId === activeProjectId.value : e.id === routedSurfaceId.value
  const next = e.type === 'project' ? tabsStore.close(e.projectId) : tabsStore.closeSurface(e.id)
  if (!wasRouted) return
  // The split's right pane is not a destination of the router: skip over it.
  const target = next && next.type === 'surface' && next.id === tabsStore.splitId ? null : next
  tabsStore.focusedPane = 'left'
  void goToEntry(target)
}

function popOut(e: TabEntry, ev: MouseEvent) {
  ev.stopPropagation()
  if (e.type === 'surface') void popOutSurface(e.id)
}

function toggleRight(e: TabEntry, ev: MouseEvent) {
  ev.stopPropagation()
  if (e.type !== 'surface') return
  if (tabsStore.splitId === e.id) {
    tabsStore.unsplit()
    return
  }
  // Moving the routed surface to the right leaves the left on another tab.
  const wasRouted = e.id === routedSurfaceId.value
  tabsStore.split(e.id)
  if (wasRouted) {
    const other = tabsStore.entries.find((x) => tabEntryKey(x) !== e.id)
    void goToEntry(other ?? null)
  }
}

// --- Drag & drop reorder ---------------------------------------------------
// The tab strip is a single-row case of the shared sortable; the whole tab is
// the handle (a tab is too small for a separate grip), so a press only becomes
// a drag past a few pixels of travel — below that it stays an ordinary click.
const stripRef = ref<HTMLElement | null>(null)
const { dragFrom, start: startDrag, itemStyle: tabStyle, clickWasDrag } = useDragSort({
  container: stripRef,
  onDrop: (from, to) => tabsStore.move(from, to),
})

/** Click after a drag means "drop here", not "switch to this tab". */
function onTabClick(e: TabEntry) {
  if (clickWasDrag()) return
  select(e)
}
</script>

<template>
  <!-- Lives inside the app title bar (see App.vue), which owns the row's height,
       background and bottom border — hence none of those here. -->
  <div
    v-if="tabsStore.entries.length > 0"
    ref="stripRef"
    class="flex items-center gap-1 min-w-0 flex-1 h-full overflow-x-auto overflow-y-hidden"
  >
    <button
      v-for="(tab, index) in tabsStore.entries"
      :key="tabEntryKey(tab)"
      type="button"
      :data-drag-index="index"
      class="group relative flex h-7 min-w-0 shrink items-center gap-2 pl-3 pr-2 rounded-md text-[13px] max-w-[200px] transition-colors select-none touch-none"
      :class="[
        dragFrom === index
          ? 'bg-accent text-foreground shadow-md cursor-grabbing'
          : isActive(tab)
            ? 'bg-accent text-foreground cursor-pointer'
            : isVisible(tab)
              ? 'bg-accent/50 text-foreground cursor-pointer'
              : 'text-muted-foreground hover:text-foreground hover:bg-accent/50 cursor-pointer',
      ]"
      :style="tabStyle(index)"
      :title="`${tab.type === 'project' ? projectName(tab.projectId) : surfaceLabel(tab)} — ${t('misc.tabs.dragToReorder')}`"
      @pointerdown="startDrag(index, $event)"
      @click="onTabClick(tab)"
    >
      <template v-if="tab.type === 'project'">
        <!-- status dot: running / awaiting permission -->
        <span class="relative flex h-2 w-2 shrink-0">
          <span
            v-if="stateOf(tab.projectId).running"
            class="absolute inline-flex h-full w-full rounded-full bg-primary opacity-60 animate-ping"
          />
          <span
            class="relative inline-flex h-2 w-2 rounded-full"
            :class="stateOf(tab.projectId).pending > 0
              ? 'bg-warning'
              : stateOf(tab.projectId).running
                ? 'bg-primary'
                : 'bg-muted-foreground/40'"
          />
        </span>

        <FolderOpen class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />
        <span class="truncate">{{ projectName(tab.projectId) }}</span>

        <!-- pending-permission badge (summed across the project's runs) -->
        <span
          v-if="stateOf(tab.projectId).pending > 0"
          class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium text-warning-foreground leading-none shrink-0"
        >{{ stateOf(tab.projectId).pending }}</span>
      </template>

      <template v-else>
        <component :is="SURFACE_META[tab.surface].icon" class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />
        <span class="truncate">{{ surfaceLabel(tab) }}</span>
        <Columns2
          v-if="tab.id === tabsStore.splitId"
          class="h-3 w-3 shrink-0 text-primary"
          :stroke-width="2"
          :aria-label="t('surfaces.tabs.focusRight')"
        />

        <!-- show on the right / pop out -->
        <span
          class="flex h-4 w-4 items-center justify-center rounded shrink-0 opacity-0 group-hover:opacity-100 hover:bg-foreground/10 transition-opacity"
          role="button"
          :title="tab.id === tabsStore.splitId ? t('surfaces.tabs.unsplit') : t('surfaces.tabs.splitRight')"
          :aria-label="tab.id === tabsStore.splitId ? t('surfaces.tabs.unsplit') : t('surfaces.tabs.splitRight')"
          @pointerdown.stop
          @click="toggleRight(tab, $event)"
        >
          <Columns2 class="h-3 w-3" :stroke-width="2" />
        </span>
        <span
          class="flex h-4 w-4 items-center justify-center rounded shrink-0 opacity-0 group-hover:opacity-100 hover:bg-foreground/10 transition-opacity"
          role="button"
          :title="t('surfaces.tabs.popOut')"
          :aria-label="t('surfaces.tabs.popOut')"
          @pointerdown.stop
          @click="popOut(tab, $event)"
        >
          <ExternalLink class="h-3 w-3" :stroke-width="2" />
        </span>
      </template>

      <!-- close -->
      <span
        class="flex h-4 w-4 items-center justify-center rounded shrink-0 opacity-0 group-hover:opacity-100 hover:bg-foreground/10 transition-opacity"
        :class="{ 'opacity-60': isActive(tab) }"
        role="button"
        :aria-label="t('misc.tabs.closeTab')"
        @pointerdown.stop
        @click="closeTab(tab, $event)"
      >
        <X class="h-3 w-3" :stroke-width="2" />
      </span>
    </button>
  </div>
</template>
