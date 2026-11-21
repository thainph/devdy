<script setup lang="ts">
// Left pane of the Notes / Todos window, built from the shared list-sidebar
// pieces (components/listDetail):
//
//   Todos 12 [3 late]                         [+ New] [⋯]
//   ( 🔍 Search todos…  (⌘F)                       ⚙︎• )
//   Filtering: Overdue · icontruction        Clear filters   ← only while filtering
//   ─────────────────────────────────────────────────────
//   ○ New todo — Draft                                       ← only while creating
//   ICONTRUCTION  2 late                                  5
//   ○ row …
//
// The "N late" badge toggles the overdue filter. Everything else about the view
// — status (with counts), project, sort, group by project, show done (todos)
// and "Reset to defaults" — lives in the one menu behind the search box's
// sliders button (lib/itemListView; the screen owns the view and remembers its
// display part). ⋯ holds Reload, the pop-out's pin / dock, and Hide the list.
//
// Groups are collapsible (remembered in localStorage per kind); a group's 🗑
// deletes its shown items (those passing the filters) after the host confirms.
// Todos with `lateness` (lib/focusRadar) put the projects with the most late
// work first; ungrouped, each row names its project.
//
// A note row: title, a one-line preview, its age and a link mark with the
// linked-session count. A todo row: a clickable checkbox, its first line, its
// deadline (red + "Nd late" when overdue, amber when due today), age and the
// link mark.
import { computed, ref } from 'vue'
import { useListPaneSidebar } from '@/composables/useListPane'
import { useI18n } from 'vue-i18n'
import { CalendarDays, Check, Folder, Link2, RotateCw, Trash2 } from 'lucide-vue-next'
import { Banner, Button, DropdownItem, DropdownSeparator } from '@/components/ui'
import ListSidebarHeader from '@/components/listDetail/ListSidebarHeader.vue'
import ListSearchBox from '@/components/listDetail/ListSearchBox.vue'
import ListFilterSummary from '@/components/listDetail/ListFilterSummary.vue'
import ListGroupHeader from '@/components/listDetail/ListGroupHeader.vue'
import ListMenuLabel from '@/components/listDetail/ListMenuLabel.vue'
import ListMenuCheck from '@/components/listDetail/ListMenuCheck.vue'
import PopoutMenuItems from '@/components/listDetail/PopoutMenuItems.vue'
import { relativeAge } from '@/lib/itemText'
import { formatDate, ymdToDate } from '@/lib/dateValue'
import { compareByDue, dueStatus, localYmd } from '@/lib/todoDue'
import { compareGroupLateness, compareLateness, groupLateness, type FocusRadarItem } from '@/lib/focusRadar'
import {
  ITEM_NO_PROJECT,
  clearItemFilters,
  defaultItemListView,
  itemFilterSummary,
  itemSorts,
  itemStatuses,
  viewDiffersFromDefault,
  type ItemListView,
  type ItemSortMode,
  type ItemStatusFilter,
} from '@/lib/itemListView'
import { formatFullDate } from '@/lib/slackThreadUtils'
import { IS_MAC } from '@/lib/appMenu'
import type { ItemRow } from './itemRow'
import { rowClass, rowListClass, rowSubClass, rowTitleClass } from '@/components/listDetail/rowStyles'

const props = defineProps<{
  kind: 'note' | 'todo'
  /** Already filtered by the view and the search. */
  rows: ItemRow[]
  /** True when there are no items at all (vs. none matching the filters). */
  emptyStore: boolean
  selectedId: string | null
  /** The projects the filter menu offers (id + name), "No project" excluded. */
  projectOptions: { value: string; label: string }[]
  /** project id → name, for the group headers. */
  projectNames: Record<string, string>
  error: string | null
  pinned: boolean
  reloading: boolean
  /** Rendered inside the main window: no window pin / dock items. */
  embedded?: boolean
  /** Todos: lateness of each open todo by id (done / unknown ones absent). */
  lateness?: Record<string, FocusRadarItem> | null
  /** Counts behind the filter menu's statuses (lib/itemListView itemStatusCounts). */
  counts: Record<ItemStatusFilter, number>
  /** While the new-item form is open: its row pinned on top of the list. */
  draft?: { title: string; sub: string } | null
}>()

const search = defineModel<string>('search', { required: true })
const view = defineModel<ItemListView>('view', { required: true })

const emit = defineEmits<{
  select: [id: string]
  add: []
  /** The draft row was clicked: back to the new-item form. */
  openDraft: []
  toggleDone: [id: string]
  /** Delete every item of a folder: a project id, or null for "No project". */
  deleteGroup: [projectId: string | null, label: string]
  togglePin: []
  /** Pop-out: move into the main window as a tab (lib/surfaceDock). */
  dock: []
  reload: []
  retry: []
}>()

const { t, locale } = useI18n()

// Hideable per screen (⌘B) when the host provides a list pane.
const rootEl = ref<HTMLElement | null>(null)
const listPane = useListPaneSidebar(rootEl)

// ── View (the filter menu) ──────────────────────────────────────────────────

function patch(p: Partial<ItemListView>) {
  view.value = { ...view.value, ...p }
}

const STATUS_DOT: Partial<Record<ItemStatusFilter, string>> = {
  overdue: 'bg-destructive',
  today: 'bg-warning',
  'due-soon': 'bg-primary',
  waiting: 'bg-muted-foreground',
}

function statusLabel(s: ItemStatusFilter): string {
  if (!s) return t('item.filterAll')
  if (s === 'linked') return t('item.filterLinked')
  return t(`item.lateness.${s}`)
}
function projectLabel(id: string): string {
  if (!id) return t('item.allProjects')
  if (id === ITEM_NO_PROJECT) return t('item.noProject')
  return props.projectNames[id] ?? props.projectOptions.find((o) => o.value === id)?.label ?? id
}

/** Statuses of the menu: All and the ones with items, plus the picked one. */
const statuses = computed(() =>
  itemStatuses(props.kind).filter((s) => !s || props.counts[s] > 0 || view.value.status === s),
)

const SORT_LABEL: Record<ItemSortMode, string> = {
  urgency: 'item.sortByUrgency',
  due: 'item.sortByDue',
  updated: 'item.sortByUpdated',
  title: 'item.sortByTitle',
}

const projectChoices = computed(() => [
  { value: '', label: t('item.allProjects') },
  ...props.projectOptions,
  { value: ITEM_NO_PROJECT, label: t('item.noProject') },
])

const changed = computed(() => viewDiffersFromDefault(props.kind, view.value))
const summary = computed(() => itemFilterSummary(view.value, { status: statusLabel, project: projectLabel }))
const overdue = computed(() => (props.kind === 'todo' ? props.counts.overdue : 0))

function toggleOverdue() {
  patch({ status: view.value.status === 'overdue' ? '' : 'overdue' })
}
function clearFilters() {
  search.value = ''
  view.value = clearItemFilters(view.value)
}
function resetView() {
  view.value = defaultItemListView(props.kind)
}

const newTitle = computed(() =>
  props.kind === 'todo' ? `${t('item.newTodo')} (${IS_MAC ? '⌥⌘N' : 'Ctrl+Alt+N'})` : t('item.newNote'),
)

// ── Rows ────────────────────────────────────────────────────────────────────

/** Deadline badges by row id: short date + urgency (a done todo is never late). */
const dueBadges = computed(() => {
  const today = localYmd()
  const out: Record<string, { label: string; title: string; tone: string }> = {}
  for (const row of props.rows) {
    const d = ymdToDate(row.dueDate)
    if (!d) continue
    const late = props.lateness?.[row.id]
    const status = row.done
      ? 'later'
      : late
        ? late.radar === 'overdue' ? 'overdue' : late.radar === 'today' ? 'today' : 'later'
        : dueStatus(row.dueDate, today)
    const daysLate = late?.radar === 'overdue' && (late.daysLeft ?? 0) < 0 ? -late.daysLeft! : 0
    out[row.id] = {
      label: daysLate
        ? t('item.lateDays', { n: daysLate })
        : formatDate(d, locale.value, {
            day: 'numeric',
            month: 'short',
            ...(row.dueDate!.slice(0, 4) === today.slice(0, 4) ? {} : { year: 'numeric' }),
          }),
      title: t('item.dueOn', { date: formatDate(d, locale.value, { dateStyle: 'full' }) }),
      tone:
        status === 'overdue' ? 'bg-destructive/15 font-medium text-destructive'
        : status === 'today' || status === 'soon' ? 'bg-warning/15 font-medium text-warning'
        : 'text-muted-foreground',
    }
  }
  return out
})

const COLLAPSED_KEY = `items.${props.kind}.collapsed`

function loadCollapsed(): Set<string> {
  try {
    const raw = JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? '[]')
    return new Set(Array.isArray(raw) ? raw.filter((x) => typeof x === 'string') : [])
  } catch {
    return new Set()
  }
}

const collapsed = ref<Set<string>>(loadCollapsed())

function toggle(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]))
  } catch {
    /* storage unavailable */
  }
}

function compareRows(a: ItemRow, b: ItemRow): number {
  const sort = view.value.sort
  if (sort === 'urgency') {
    const late = props.lateness
    return (late ? compareLateness(late[a.id], late[b.id]) : 0) || compareByDue(a, b)
  }
  if (sort === 'due') return compareByDue(a, b)
  if (sort === 'updated') return a.at < b.at ? 1 : a.at > b.at ? -1 : 0
  // By title: untitled last.
  return (!a.title ? 1 : 0) - (!b.title ? 1 : 0) || a.title.localeCompare(b.title)
}

const flat = computed(() => !view.value.grouped)

/**
 * One header-less group when ungrouped; else rows by project, "No project"
 * last — todos with lateness: the most late project first, then A–Z.
 */
const groups = computed(() => {
  const sorted = [...props.rows].sort(compareRows)
  if (flat.value) return [{ key: '__flat__', label: '', rows: sorted, late: groupLateness([]) }]
  const late = props.lateness
  const map = new Map<string, ItemRow[]>()
  for (const row of sorted) {
    const key = row.projectId ?? ITEM_NO_PROJECT
    const list = map.get(key)
    if (list) list.push(row)
    else map.set(key, [row])
  }
  return [...map.entries()]
    .map(([key, rows]) => ({
      key,
      label: key === ITEM_NO_PROJECT ? t('item.noProject') : props.projectNames[key] ?? key,
      rows,
      late: groupLateness(late ? rows.map((r) => late[r.id]) : []),
    }))
    .sort((a, b) =>
      (a.key === ITEM_NO_PROJECT ? 1 : 0) - (b.key === ITEM_NO_PROJECT ? 1 : 0) ||
      (view.value.sort === 'urgency' ? compareGroupLateness(a.late, b.late) : 0) ||
      a.label.localeCompare(b.label),
    )
})
</script>

<template>
  <aside
    v-if="listPane.shown.value"
    ref="rootEl"
    class="list-sidebar flex shrink-0 flex-col border-r border-border/60 bg-sidebar"
    :class="[listPane.widthClass.value, listPane.floatClass.value]"
  >
    <ListSidebarHeader
      :title="kind === 'note' ? t('item.navNotes') : t('item.navTodos')"
      :count="rows.length"
      :new-label="t('common.new')"
      :new-title="newTitle"
      @new="emit('add')"
    >
      <template v-if="overdue > 0 || view.status === 'overdue'" #badge>
        <button
          type="button"
          class="h-5 shrink-0 cursor-pointer rounded-full px-2 text-[11px] font-semibold tabular-nums transition-colors"
          :class="view.status === 'overdue'
            ? 'bg-destructive text-destructive-foreground'
            : 'bg-destructive/12 text-destructive hover:bg-destructive/20'"
          :aria-pressed="view.status === 'overdue'"
          :title="t('item.lateBadgeTitle')"
          @click="toggleOverdue"
        >
          {{ t('listPane.lateCount', { n: overdue }) }}
        </button>
      </template>
      <template #menu>
        <!-- Reload from the DB (the MCP sidecar writes without events). -->
        <DropdownItem :disabled="reloading" @click="emit('reload')">
          <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': reloading }" :stroke-width="1.75" />
          {{ t('item.reload') }}
        </DropdownItem>
        <PopoutMenuItems v-if="!embedded" :pinned="pinned" @toggle-pin="emit('togglePin')" @dock="emit('dock')" />
      </template>
    </ListSidebarHeader>

    <div class="shrink-0 px-2.5 pb-1.5 pt-0.5">
      <ListSearchBox
        v-model="search"
        :placeholder="kind === 'note' ? t('item.searchNotes') : t('item.searchTodos')"
        :filter-active="changed"
      >
        <template #filters>
          <ListMenuLabel>{{ t('item.statusTitle') }}</ListMenuLabel>
          <ListMenuCheck
            v-for="s in statuses"
            :key="s || 'all'"
            radio
            :checked="view.status === s"
            :dot="STATUS_DOT[s]"
            :count="counts[s]"
            @click="patch({ status: s })"
          >
            {{ statusLabel(s) }}
          </ListMenuCheck>
          <DropdownSeparator />
          <ListMenuLabel>{{ t('item.projectLabel') }}</ListMenuLabel>
          <ListMenuCheck
            v-for="p in projectChoices"
            :key="p.value || 'all'"
            radio
            :checked="view.project === p.value"
            @click="patch({ project: p.value })"
          >
            {{ p.label }}
          </ListMenuCheck>
          <DropdownSeparator />
          <ListMenuLabel>{{ t('item.sortTitle') }}</ListMenuLabel>
          <ListMenuCheck
            v-for="s in itemSorts(kind)"
            :key="s"
            radio
            :checked="view.sort === s"
            @click="patch({ sort: s })"
          >
            {{ t(SORT_LABEL[s]) }}
          </ListMenuCheck>
          <DropdownSeparator />
          <ListMenuLabel>{{ t('item.displayTitle') }}</ListMenuLabel>
          <ListMenuCheck :checked="view.grouped" @click="patch({ grouped: !view.grouped })">
            {{ t('item.groupByProject') }}
          </ListMenuCheck>
          <ListMenuCheck v-if="kind === 'todo'" :checked="view.showDone" @click="patch({ showDone: !view.showDone })">
            {{ t('item.showDone') }}
          </ListMenuCheck>
          <DropdownSeparator />
          <DropdownItem :disabled="!changed" @click="resetView">
            <span class="w-3.5 shrink-0" aria-hidden="true" />
            {{ t('listPane.resetView') }}
          </DropdownItem>
        </template>
      </ListSearchBox>
    </div>

    <ListFilterSummary :parts="summary" @clear="clearFilters" />

    <!-- Rows -->
    <div class="min-h-0 flex-1 overflow-y-auto border-t border-border/40 px-2 pb-2">
      <Banner v-if="error" tone="destructive" class="mb-2 mt-2">
        {{ error }}
        <template #actions>
          <Button
            variant="ghost"
            size="icon-xs"
            :title="t('item.reload')"
            :aria-label="t('item.reload')"
            @click="emit('retry')"
          >
            <RotateCw class="h-3 w-3" :stroke-width="2" />
          </Button>
        </template>
      </Banner>

      <!-- The item being created, where it will land. -->
      <button
        v-if="draft"
        type="button"
        class="mt-1.5 cursor-pointer gap-2 pl-2 pr-2 text-left"
        :class="rowClass(true)"
        @click="emit('openDraft'); listPane.picked()"
      >
        <span
          v-if="kind === 'todo'"
          class="mt-0.5 h-3.5 w-3.5 shrink-0 rounded-[4px] border border-dashed border-primary"
          aria-hidden="true"
        />
        <span class="flex min-w-0 flex-1 flex-col">
          <span
            class="truncate"
            :class="[rowTitleClass, !draft.title && 'font-normal italic text-muted-foreground']"
          >
            {{ draft.title || (kind === 'note' ? t('item.create.metaNote') : t('item.create.metaTodo')) }}
          </span>
          <span :class="rowSubClass">{{ draft.sub }}</span>
        </span>
        <span class="shrink-0 rounded-full bg-primary/15 px-1.5 text-[10px] font-semibold text-primary">
          {{ t('item.draft') }}
        </span>
      </button>

      <div v-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">
          {{ kind === 'note' ? t('item.emptyNote') : t('item.emptyTodo') }}
        </p>
        <p class="mt-1 text-[11px] text-muted-foreground/70">
          {{ kind === 'note' ? t('item.emptyNoteHint') : t('item.emptyTodoHint') }}
        </p>
      </div>
      <div v-else-if="rows.length === 0" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        <p>{{ t('item.noResults') }}</p>
        <button
          v-if="search || summary.length"
          type="button"
          class="mt-1 cursor-pointer underline underline-offset-2 hover:text-foreground"
          @click="clearFilters"
        >
          {{ t('listPane.clearFilters') }}
        </button>
      </div>

      <template v-for="g in groups" v-else :key="g.key">
        <ListGroupHeader
          v-if="!flat"
          :label="g.label"
          :count="g.rows.length"
          :late="g.late.overdue"
          :late-title="t('item.groupLate', { n: g.late.overdue, days: g.late.worstLateDays ?? 0 })"
          :collapsed="collapsed.has(g.key)"
          @toggle="toggle(g.key)"
        >
          <template #actions>
            <Button
              variant="destructive-ghost"
              size="icon-xs"
              class="opacity-0 transition-opacity focus-visible:opacity-100 group-hover/folder:opacity-100"
              :title="t('item.deleteFolder')"
              :aria-label="t('item.deleteFolder')"
              @click="emit('deleteGroup', g.key === ITEM_NO_PROJECT ? null : g.key, g.label)"
            >
              <Trash2 class="h-3 w-3" :stroke-width="1.75" />
            </Button>
          </template>
        </ListGroupHeader>

        <ul v-if="flat || !collapsed.has(g.key)" :class="[rowListClass, flat ? 'mt-1.5' : 'mb-2']">
          <li v-for="row in g.rows" :key="row.id">
            <div class="group/row gap-2 pl-2 pr-2" :class="rowClass(row.id === selectedId)">
              <button
                v-if="kind === 'todo'"
                type="button"
                class="mt-0.5 flex h-3.5 w-3.5 shrink-0 cursor-pointer items-center justify-center rounded-[4px] border transition-colors"
                :class="row.done
                  ? 'border-primary bg-primary text-primary-foreground'
                  : 'border-muted-foreground/40 hover:border-primary'"
                :aria-pressed="row.done"
                :title="row.done ? t('item.markNotDone') : t('item.markDone')"
                :aria-label="row.done ? t('item.markNotDone') : t('item.markDone')"
                @click="emit('toggleDone', row.id)"
              >
                <Check v-if="row.done" class="h-2 w-2" :stroke-width="3" />
              </button>
              <button
                type="button"
                class="flex min-w-0 flex-1 cursor-pointer flex-col text-left"
                :title="row.title"
                @click="emit('select', row.id); listPane.picked()"
              >
                <span class="flex w-full min-w-0 items-center gap-1.5">
                  <span
                    class="min-w-0 flex-1 truncate"
                    :class="[rowTitleClass, row.done && 'text-muted-foreground line-through']"
                  >
                    {{ row.title || t('item.untitled') }}
                  </span>
                  <span
                    v-if="dueBadges[row.id]"
                    class="flex shrink-0 items-center gap-0.5 rounded px-1 text-[10px] tabular-nums"
                    :class="dueBadges[row.id].tone"
                    :title="dueBadges[row.id].title"
                  >
                    <CalendarDays class="h-3 w-3" :stroke-width="2" />
                    {{ dueBadges[row.id].label }}
                  </span>
                  <span
                    v-if="row.linkedRunCount > 0"
                    class="flex shrink-0 items-center gap-0.5 text-[10px] tabular-nums text-muted-foreground"
                    :title="t('sessionLink.linkedCount', row.linkedRunCount)"
                  >
                    <Link2 class="h-3 w-3" :stroke-width="2" />
                    <template v-if="row.linkedRunCount > 1">{{ row.linkedRunCount }}</template>
                  </span>
                  <span class="shrink-0 text-[10px] tabular-nums text-muted-foreground/80" :title="formatFullDate(row.at)">
                    {{ relativeAge(row.at) }}
                  </span>
                </span>
                <!-- Ungrouped: the project leads the sub line, then a note's preview. -->
                <span v-if="flat" class="flex w-full min-w-0 items-center gap-1" :class="rowSubClass">
                  <Folder class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                  <span class="max-w-[45%] shrink-0 truncate">
                    {{ row.projectId ? projectNames[row.projectId] ?? row.projectId : t('item.noProject') }}
                  </span>
                  <template v-if="row.preview">
                    <span class="shrink-0">·</span>
                    <span class="min-w-0 truncate">{{ row.preview }}</span>
                  </template>
                </span>
                <span v-else-if="row.preview" class="block w-full" :class="rowSubClass">
                  {{ row.preview }}
                </span>
              </button>
            </div>
          </li>
        </ul>
      </template>
    </div>
  </aside>
</template>
