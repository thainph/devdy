<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, Columns2, ExternalLink, PanelRightClose, X, XSquare } from 'lucide-vue-next'
import { Button, ContextMenu, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
import WorkspaceTab from '@/components/WorkspaceTab.vue'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useProjectsStore } from '@/stores/projects'
import { useRunsStore } from '@/stores/runs'
import { useDragSort } from '@/composables/useDragSort'
import { entryIcon, entryLabel } from '@/lib/tabMeta'
import { tabStripLayout } from '@/lib/tabStripLayout'
import { popOutSurface, routedKey } from '@/lib/surfaceOpen'
import { canSplit, closeEntry, closeOtherEntries, selectEntry, toggleSplit } from '@/lib/tabActions'

/**
 * The shared tab strip of the main window: one tab per open PROJECT (its runs
 * are switched from RunView's History list; the tab returns to the last-viewed
 * run), PAGE (Dashboard, Settings…) and SURFACE — PR Inbox, Calendar, Slack
 * threads, Todos, a file… shown in this window instead of their own
 * (lib/surfaces). A surface tab can pop out into its own window.
 *
 * Split view is a mode, toggled by the button at the strip's end (⌘\): on,
 * a session tab shows in the left half and any other tab in the right half
 * (lib/tabActions). Both tabs on screen are marked, the focused one lifted.
 *
 * Aggregate live status (any run streaming / awaiting permission) is read from
 * liveRuns.
 */

const { t } = useI18n()
const tabsStore = useWorkspaceTabsStore()
const live = useLiveRunsStore()
const projectsStore = useProjectsStore()
const runsStore = useRunsStore()

/** The tab the router shows (reactive: reads the router's current route). */
const routed = computed(() => routedKey())

function isActive(e: TabEntry): boolean {
  const key = tabEntryKey(e)
  if (tabsStore.isSplit) {
    return tabsStore.focusedPane === 'right' ? key === tabsStore.splitId : key === routed.value
  }
  return key === routed.value
}

/** Shown in the half that doesn't have the focus — dimmer than active, still marked. */
function isVisible(e: TabEntry): boolean {
  const key = tabEntryKey(e)
  return key === routed.value || key === tabsStore.splitId
}

function look(e: TabEntry): 'active' | 'visible' | 'idle' {
  return isActive(e) ? 'active' : isVisible(e) ? 'visible' : 'idle'
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

function stateOf(e: TabEntry): ProjectState {
  return (e.type === 'project' && states.value[e.projectId]) || { running: false, pending: 0 }
}

// Right-click menu of a tab.
const menu = ref<{ open: boolean; x: number; y: number; tab: TabEntry | null }>({ open: false, x: 0, y: 0, tab: null })
function openMenu(e: TabEntry, ev: MouseEvent) {
  menu.value = { open: true, x: ev.clientX, y: ev.clientY, tab: e }
}
function closeMenu() {
  menu.value.open = false
}
function runMenu(action: () => void) {
  closeMenu()
  action()
}

function popOut(e: TabEntry) {
  if (e.type === 'surface') void popOutSurface(e.id)
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
  const active = all.findIndex(isActive)
  const current = active >= 0 ? active : all.findIndex(isVisible)
  return tabStripLayout(
    all.map(() => 1),
    avail.value,
    current,
    { min: TAB_MIN, max: TAB_MAX, gap: GAP, more: MORE_W },
  )
})

const shownTabs = computed(() => layout.value.shown.map((i) => tabsStore.entries[i]).filter(Boolean))
const hiddenCount = computed(() => layout.value.hidden.length)

const tabWidth = computed(() => ({ width: `${layout.value.width}px` }))

// --- Drag & drop reorder ---------------------------------------------------
// The tab strip is a single-row case of the shared sortable; the whole tab is
// the handle, so a press only becomes a drag past a few
// pixels of travel — below that it stays an ordinary click.
const stripRef = ref<HTMLElement | null>(null)
const { dragFrom, start: startDrag, itemStyle: tabStyle, clickWasDrag } = useDragSort({
  container: stripRef,
  // Indices are positions among the SHOWN units; map them back to the strip.
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

const splitLabel = computed(() =>
  tabsStore.isSplit ? t('surfaces.tabs.splitOff') : canSplit() ? t('surfaces.tabs.splitOn') : t('surfaces.tabs.splitNeedsSession'),
)
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
    <WorkspaceTab
      v-for="(tab, index) in shownTabs"
      :key="tabEntryKey(tab)"
      :data-drag-index="index"
      :tab="tab"
      :label="label(tab)"
      :running="stateOf(tab).running"
      :pending="stateOf(tab).pending"
      :look="dragFrom === index ? 'active' : look(tab)"
      :class="dragFrom === index ? 'shadow-md !cursor-grabbing' : ''"
      :style="[tabWidth, tabStyle(index)]"
      :title="`${label(tab)} — ${t('misc.tabs.dragToReorder')}`"
      @pointerdown="startDrag(index, $event)"
      @click="onTabClick(tab)"
      @contextmenu.prevent="openMenu(tab, $event)"
      @close="closeEntry(tab)"
    />
  </div>

  <ContextMenu :open="menu.open" :x="menu.x" :y="menu.y" @close="closeMenu">
    <template v-if="menu.tab">
      <DropdownItem v-if="menu.tab.type === 'surface'" @click="runMenu(() => popOut(menu.tab!))">
        <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('surfaces.tabs.popOut') }}
      </DropdownItem>
      <DropdownSeparator />
      <DropdownItem @click="runMenu(() => closeEntry(menu.tab!))">
        <X class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('misc.tabs.closeTab') }}
      </DropdownItem>
      <DropdownItem :disabled="tabsStore.entries.length < 2" @click="runMenu(() => void closeOtherEntries(menu.tab!))">
        <XSquare class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('misc.tabs.closeOthers') }}
      </DropdownItem>
    </template>
  </ContextMenu>

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
          v-if="stateOf(entry).pending > 0"
          class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium text-warning-foreground leading-none shrink-0"
        >{{ stateOf(entry).pending }}</span>
        <span
          class="flex h-4 w-4 items-center justify-center rounded shrink-0 text-muted-foreground hover:bg-foreground/10 hover:text-foreground"
          role="button"
          :aria-label="t('misc.tabs.closeTab')"
          @click.stop="closeEntry(entry)"
        >
          <X class="h-3 w-3" :stroke-width="2" />
        </span>
      </DropdownItem>
    </div>
  </DropdownMenu>

  <!-- Split view on / off (⌘\). -->
  <Button
    variant="ghost"
    size="icon-sm"
    class="shrink-0"
    :class="tabsStore.isSplit && 'bg-primary/15 text-primary hover:bg-primary/25 hover:text-primary'"
    :disabled="!canSplit()"
    :title="splitLabel"
    :aria-label="splitLabel"
    :aria-pressed="tabsStore.isSplit"
    @click="toggleSplit"
  >
    <component :is="tabsStore.isSplit ? PanelRightClose : Columns2" class="h-3.5 w-3.5" :stroke-width="1.75" />
  </Button>
  </div>
</template>
