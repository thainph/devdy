<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'
import { ChevronDown, Columns2, ExternalLink, FolderOpen, X } from 'lucide-vue-next'
import { DropdownItem, DropdownMenu } from '@/components/ui'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useProjectsStore } from '@/stores/projects'
import { useRunsStore } from '@/stores/runs'
import { useDragSort } from '@/composables/useDragSort'
import { entryIcon, entryLabel } from '@/lib/tabMeta'
import { tabStripLayout } from '@/lib/tabStripLayout'
import { surfaceForPath, surfaceTabId, cleanParams } from '@/lib/surfaces'
import { popOutSurface } from '@/lib/surfaceOpen'
import { closeEntry, selectEntry, toggleSplitEntry } from '@/lib/tabActions'

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
  if (e.type === 'page') return e.path === route.path && tabsStore.focusedPane === 'left'
  if (e.id === tabsStore.splitId) return tabsStore.focusedPane === 'right'
  return e.id === routedSurfaceId.value && tabsStore.focusedPane === 'left'
}

/** Shown in the half that doesn't have the focus — dimmer than active, still marked. */
function isVisible(e: TabEntry): boolean {
  if (e.type === 'project') return e.projectId === activeProjectId.value
  if (e.type === 'page') return e.path === route.path
  return e.id === tabsStore.splitId || e.id === routedSurfaceId.value
}

function projectName(projectId: string): string {
  return projectsStore.projects.find((p) => p.id === projectId)?.name ?? t('misc.tabs.project')
}

function label(e: TabEntry): string {
  return entryLabel(e, t, projectName)
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

function closeTab(e: TabEntry, ev?: MouseEvent) {
  ev?.stopPropagation()
  closeEntry(e)
}

function popOut(e: TabEntry, ev: MouseEvent) {
  ev.stopPropagation()
  if (e.type === 'surface') void popOutSurface(e.id)
}

function toggleRight(e: TabEntry, ev: MouseEvent) {
  ev.stopPropagation()
  if (e.type === 'surface') toggleSplitEntry(e)
}

// --- Overflow (Chrome-style, lib/tabStripLayout) ------------------------------
// Tabs never shrink below TAB_MIN so a title stays readable; the rest hide
// behind the ▾ button, which lists every tab.
const TAB_MIN = 140
const TAB_MAX = 200
const GAP = 4
const MORE_W = 52

const wrapRef = ref<HTMLElement | null>(null)
const avail = ref(0)
let resize: ResizeObserver | null = null
onMounted(() => {
  resize = new ResizeObserver(([entry]) => {
    avail.value = entry.contentRect.width
  })
  if (wrapRef.value) resize.observe(wrapRef.value)
})
onBeforeUnmount(() => resize?.disconnect())
watch(wrapRef, (el, old) => {
  if (old) resize?.unobserve(old)
  if (el) resize?.observe(el)
})

const layout = computed(() => {
  const all = tabsStore.entries
  const active = all.findIndex((e) => isActive(e))
  const current = active >= 0 ? active : all.findIndex((e) => isVisible(e))
  return tabStripLayout(all.length, avail.value, current, { min: TAB_MIN, max: TAB_MAX, gap: GAP, more: MORE_W })
})

const shownTabs = computed(() => layout.value.shown.map((i) => ({ tab: tabsStore.entries[i] })))
const hiddenCount = computed(() => layout.value.hidden.length)

// --- Drag & drop reorder ---------------------------------------------------
// The tab strip is a single-row case of the shared sortable; the whole tab is
// the handle (a tab is too small for a separate grip), so a press only becomes
// a drag past a few pixels of travel — below that it stays an ordinary click.
const stripRef = ref<HTMLElement | null>(null)
const { dragFrom, start: startDrag, itemStyle: tabStyle, clickWasDrag } = useDragSort({
  container: stripRef,
  // Indices are positions among the SHOWN tabs; map them back to the strip.
  onDrop: (from, to) => {
    const { shown } = layout.value
    if (shown[from] !== undefined && shown[to] !== undefined) tabsStore.move(shown[from], shown[to])
  },
})

/** Click after a drag means "drop here", not "switch to this tab". */
function onTabClick(e: TabEntry) {
  if (clickWasDrag()) return
  selectEntry(e)
}
</script>

<template>
  <!-- Lives inside the app title bar (see App.vue), which owns the row's height,
       background and bottom border — hence none of those here. -->
  <div
    v-if="tabsStore.entries.length > 0"
    ref="wrapRef"
    class="flex items-center gap-1 min-w-0 flex-1 h-full overflow-hidden"
  >
  <div ref="stripRef" class="flex items-center gap-1 min-w-0 h-full overflow-hidden">
    <button
      v-for="({ tab }, index) in shownTabs"
      :key="tabEntryKey(tab)"
      type="button"
      :data-drag-index="index"
      class="group relative flex h-7 shrink-0 items-center gap-2 pl-3 pr-2 rounded-md text-[13px] transition-colors select-none touch-none"
      :class="[
        dragFrom === index
          ? 'bg-accent text-foreground shadow-md cursor-grabbing'
          : isActive(tab)
            ? 'bg-accent text-foreground cursor-pointer'
            : isVisible(tab)
              ? 'bg-accent/50 text-foreground cursor-pointer'
              : 'text-muted-foreground hover:text-foreground hover:bg-accent/50 cursor-pointer',
      ]"
      :style="[{ width: `${layout.width}px` }, tabStyle(index)]"
      :title="`${label(tab)} — ${t('misc.tabs.dragToReorder')}`"
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
        <span class="min-w-0 flex-1 truncate text-left">{{ projectName(tab.projectId) }}</span>

        <!-- pending-permission badge (summed across the project's runs) -->
        <span
          v-if="stateOf(tab.projectId).pending > 0"
          class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium text-warning-foreground leading-none shrink-0"
        >{{ stateOf(tab.projectId).pending }}</span>
      </template>

      <template v-else-if="tab.type === 'page'">
        <component :is="entryIcon(tab)" class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />
        <span class="min-w-0 flex-1 truncate text-left">{{ label(tab) }}</span>
      </template>

      <template v-else>
        <component :is="entryIcon(tab)" class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />
        <span class="min-w-0 flex-1 truncate text-left">{{ label(tab) }}</span>
        <Columns2
          v-if="tab.id === tabsStore.splitId"
          class="h-3 w-3 shrink-0 text-primary"
          :stroke-width="2"
          :aria-label="t('surfaces.tabs.focusRight')"
        />

        <!-- show on the right / pop out -->
        <span
          class="hidden h-4 w-4 items-center justify-center rounded shrink-0 group-hover:flex hover:bg-foreground/10"
          role="button"
          :title="tab.id === tabsStore.splitId ? t('surfaces.tabs.unsplit') : t('surfaces.tabs.splitRight')"
          :aria-label="tab.id === tabsStore.splitId ? t('surfaces.tabs.unsplit') : t('surfaces.tabs.splitRight')"
          @pointerdown.stop
          @click="toggleRight(tab, $event)"
        >
          <Columns2 class="h-3 w-3" :stroke-width="2" />
        </span>
        <span
          class="hidden h-4 w-4 items-center justify-center rounded shrink-0 group-hover:flex hover:bg-foreground/10"
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

  <!-- Every tab, for the ones hidden past the edge (Chrome's tab list). -->
  <DropdownMenu v-if="hiddenCount > 0" align="right" class="shrink-0">
    <template #trigger>
      <button
        type="button"
        class="flex h-7 items-center gap-1 rounded-md px-2 text-[11px] font-medium text-muted-foreground hover:bg-accent/50 hover:text-foreground cursor-pointer"
        :title="t('misc.tabs.allTabs', { n: hiddenCount })"
        :aria-label="t('misc.tabs.allTabs', { n: hiddenCount })"
      >
        +{{ hiddenCount }}
        <ChevronDown class="h-3.5 w-3.5" :stroke-width="2" />
      </button>
    </template>
    <div class="max-h-[60vh] w-72 overflow-y-auto">
      <DropdownItem
        v-for="entry in tabsStore.entries"
        :key="tabEntryKey(entry)"
        :class="isActive(entry) ? 'bg-accent' : ''"
        @click="selectEntry(entry)"
      >
        <component :is="entryIcon(entry)" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="min-w-0 flex-1 truncate">{{ label(entry) }}</span>
        <span
          v-if="entry.type === 'project' && stateOf(entry.projectId).pending > 0"
          class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium text-warning-foreground leading-none shrink-0"
        >{{ stateOf(entry.projectId).pending }}</span>
        <span
          class="flex h-4 w-4 items-center justify-center rounded shrink-0 text-muted-foreground hover:bg-foreground/10 hover:text-foreground"
          role="button"
          :aria-label="t('misc.tabs.closeTab')"
          @click.stop="closeTab(entry, $event)"
        >
          <X class="h-3 w-3" :stroke-width="2" />
        </span>
      </DropdownItem>
    </div>
  </DropdownMenu>
  </div>
</template>
