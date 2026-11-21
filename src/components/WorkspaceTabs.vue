<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowLeftRight, ChevronDown, Columns2, PanelRightOpen, X } from 'lucide-vue-next'
import { ContextMenu, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
import WorkspaceTab from '@/components/WorkspaceTab.vue'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useProjectsStore } from '@/stores/projects'
import { useRunsStore } from '@/stores/runs'
import { useDragSort } from '@/composables/useDragSort'
import { entryIcon, entryLabel } from '@/lib/tabMeta'
import { tabStripLayout, unitWidth } from '@/lib/tabStripLayout'
import { stripUnits } from '@/lib/workspaceState'
import { popOutSurface, routedKey } from '@/lib/surfaceOpen'
import { canOpenOnRight, closeEntry, openOnRight, selectEntry, swapSplit, unsplitFocused } from '@/lib/tabActions'

/**
 * The shared tab strip of the main window: one tab per open PROJECT (its runs
 * are switched from RunView's History list; the tab returns to the last-viewed
 * run), PAGE (Dashboard, Settings…) and SURFACE — PR Inbox, Calendar, Slack
 * threads, Todos, a file… shown in this window instead of their own
 * (lib/surfaces). A surface tab can pop out into its own window.
 *
 * Split view, one rule: the left half is the screen you're on, the right half
 * the tab you "Open on the right" (hover button or right-click); doing it on
 * another tab while split replaces the right half. The two then sit side by
 * side on the strip as ONE merged tab with two halves, the focused half lifted,
 * with swap (⇄) and close-split buttons. Clicking another tab shows it full width
 * while the pair stays (idle look); clicking a half brings the split back with
 * that half focused. The pair drags as a unit; closing a half ends the split.
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
  const pair = tabsStore.pair
  if (tabsStore.isSplit && pair) {
    return (key === pair.left && tabsStore.focusedPane === 'left') || (key === pair.right && tabsStore.focusedPane === 'right')
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

/** In the shown pair the button ends the split; elsewhere it opens the tab on the right. */
function splitAction(e: TabEntry, inPair: boolean): 'open' | 'close' | null {
  if (inPair) return tabsStore.pairShown ? 'close' : null
  return canOpenOnRight(e) ? 'open' : null
}
function onSplitButton(e: TabEntry, inPair: boolean) {
  if (inPair) unsplitFocused()
  else openOnRight(e)
}

// Right-click menu of a tab: the same split actions, spelled out.
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
const menuInShownPair = computed(() => !!menu.value.tab && tabsStore.isSplit && inPair(menu.value.tab))

function popOut(e: TabEntry) {
  if (e.type === 'surface') void popOutSurface(e.id)
}

// --- Units: tabs, and the pair as one merged tab -------------------------------
type Unit = { kind: 'tab'; tab: TabEntry } | { kind: 'pair'; left: TabEntry; right: TabEntry }

const units = computed<Unit[]>(() => {
  const all = tabsStore.entries
  return stripUnits(all.map(tabEntryKey), tabsStore.pair).map((u) =>
    u.length === 2 ? { kind: 'pair', left: all[u[0]], right: all[u[1]] } : { kind: 'tab', tab: all[u[0]] },
  )
})

function unitEntries(u: Unit): TabEntry[] {
  return u.kind === 'pair' ? [u.left, u.right] : [u.tab]
}

function unitKey(u: Unit): string {
  return u.kind === 'pair' ? `pair:${tabEntryKey(u.left)}|${tabEntryKey(u.right)}` : tabEntryKey(u.tab)
}

// --- Overflow (Chrome-style, lib/tabStripLayout) ------------------------------
// Tabs never shrink below TAB_MIN so a title stays readable; the rest hide
// behind the ▾ button, which lists every tab. The pair takes two slots.
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
  const all = units.value
  const active = all.findIndex((u) => unitEntries(u).some(isActive))
  const current = active >= 0 ? active : all.findIndex((u) => unitEntries(u).some(isVisible))
  return tabStripLayout(
    all.map((u) => (u.kind === 'pair' ? 2 : 1)),
    avail.value,
    current,
    { min: TAB_MIN, max: TAB_MAX, gap: GAP, more: MORE_W },
  )
})

const shownUnits = computed(() => layout.value.shown.map((i) => units.value[i]).filter(Boolean))
const hiddenCount = computed(() =>
  layout.value.hidden.reduce((n, i) => n + (units.value[i] ? unitEntries(units.value[i]).length : 0), 0),
)

function unitStyle(u: Unit) {
  return { width: `${u.kind === 'pair' ? unitWidth(2, layout.value.width, GAP) : layout.value.width}px` }
}

// --- Drag & drop reorder ---------------------------------------------------
// The tab strip is a single-row case of the shared sortable; the whole tab (or
// the whole pair) is the handle, so a press only becomes a drag past a few
// pixels of travel — below that it stays an ordinary click.
const stripRef = ref<HTMLElement | null>(null)
const { dragFrom, start: startDrag, itemStyle: tabStyle, clickWasDrag } = useDragSort({
  container: stripRef,
  // Indices are positions among the SHOWN units; map them back to the strip.
  onDrop: (from, to) => {
    const { shown } = layout.value
    if (shown[from] !== undefined && shown[to] !== undefined) tabsStore.moveUnit(shown[from], shown[to])
  },
})

/** Click after a drag means "drop here", not "switch to this tab". */
function onTabClick(e: TabEntry) {
  if (clickWasDrag()) return
  selectEntry(e)
}

function inPair(e: TabEntry): boolean {
  const key = tabEntryKey(e)
  return key === tabsStore.pair?.left || key === tabsStore.pair?.right
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
    <template v-for="(unit, index) in shownUnits" :key="unitKey(unit)">
      <WorkspaceTab
        v-if="unit.kind === 'tab'"
        :data-drag-index="index"
        :tab="unit.tab"
        :label="label(unit.tab)"
        :running="stateOf(unit.tab).running"
        :pending="stateOf(unit.tab).pending"
        :look="dragFrom === index ? 'active' : look(unit.tab)"
        :split-action="splitAction(unit.tab, false)"
        :class="dragFrom === index ? 'shadow-md !cursor-grabbing' : ''"
        :style="[unitStyle(unit), tabStyle(index)]"
        :title="`${label(unit.tab)} — ${t('misc.tabs.dragToReorder')}`"
        @pointerdown="startDrag(index, $event)"
        @click="onTabClick(unit.tab)"
        @contextmenu.prevent="openMenu(unit.tab, $event)"
        @close="closeEntry(unit.tab)"
        @toggle-split="onSplitButton(unit.tab, false)"
        @pop-out="popOut(unit.tab)"
      />
      <!-- The split pair: both tabs in one frame, the focused half lifted; an
           idle frame while another tab is shown full width. -->
      <div
        v-else
        :data-drag-index="index"
        role="group"
        :aria-label="t('surfaces.tabs.pair', { left: label(unit.left), right: label(unit.right) })"
        class="flex h-7 shrink-0 items-stretch gap-0.5 rounded-lg p-0.5 select-none touch-none transition-colors"
        :class="[
          tabsStore.pairShown
            ? 'bg-primary/10 shadow-[inset_0_0_0_1px_hsl(var(--primary)/0.4)]'
            : 'shadow-[inset_0_0_0_1px_hsl(var(--border))]',
          dragFrom === index ? 'shadow-md cursor-grabbing' : '',
        ]"
        :style="[unitStyle(unit), tabStyle(index)]"
        :title="`${t('surfaces.tabs.pair', { left: label(unit.left), right: label(unit.right) })} — ${t('misc.tabs.dragToReorder')}`"
        @pointerdown="startDrag(index, $event)"
      >
        <WorkspaceTab
          v-for="half in [unit.left, unit.right]"
          :key="tabEntryKey(half)"
          half
          :tab="half"
          :label="label(half)"
          :running="stateOf(half).running"
          :pending="stateOf(half).pending"
          :look="tabsStore.pairShown ? (isActive(half) ? 'active' : 'visible') : 'idle'"
          :split-action="splitAction(half, true)"
          :swappable="tabsStore.pairShown"
          :title="label(half)"
          @click="onTabClick(half)"
          @contextmenu.prevent="openMenu(half, $event)"
          @close="closeEntry(half)"
          @toggle-split="onSplitButton(half, true)"
          @swap="swapSplit()"
          @pop-out="popOut(half)"
        />
      </div>
    </template>
  </div>

  <ContextMenu :open="menu.open" :x="menu.x" :y="menu.y" @close="closeMenu">
    <template v-if="menu.tab">
      <template v-if="menuInShownPair">
        <DropdownItem @click="runMenu(swapSplit)">
          <ArrowLeftRight class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          {{ t('surfaces.tabs.swapSplit') }}
        </DropdownItem>
        <DropdownItem @click="runMenu(unsplitFocused)">
          <Columns2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          {{ t('surfaces.tabs.unsplit') }}
        </DropdownItem>
      </template>
      <DropdownItem v-else :disabled="!canOpenOnRight(menu.tab)" @click="runMenu(() => openOnRight(menu.tab!))">
        <PanelRightOpen class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('surfaces.tabs.openOnRight') }}
      </DropdownItem>
      <DropdownSeparator />
      <DropdownItem @click="runMenu(() => closeEntry(menu.tab!))">
        <X class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('misc.tabs.closeTab') }}
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
        <!-- A half of the split pair. -->
        <Columns2
          v-if="inPair(entry)"
          class="h-3 w-3 shrink-0"
          :class="tabsStore.pairShown ? 'text-primary' : 'text-muted-foreground'"
          :stroke-width="2"
          :aria-label="t('surfaces.tabs.pair', { left: label(tabsStore.entry(tabsStore.pair?.left) ?? entry), right: label(tabsStore.entry(tabsStore.pair?.right) ?? entry) })"
        />
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
  </div>
</template>
