<script setup lang="ts">
// Left pane of the Notes / Todos window — the sibling of SlackSidebar, same
// layout: search + the window's pin / reload (no custom titlebar), project
// filter + "+ New" (as wide as pin + reload, so the select lines up with the
// search box), a tab row (notes: All | Linked to a session; todos: the
// lateness tabs) ending in one ⇅ menu (the sort, todos also "Show done"), then
// the items grouped by project (each header collapsible, remembered in
// localStorage per kind; its 🗑 deletes the whole folder's shown items — those
// passing the filters — after the host confirms).
//
// A note row: title, a one-line preview, its age and a link mark with the
// linked-session count. A todo row: a clickable checkbox, its first line, its
// deadline (red + "Nd late" when overdue, amber when due today), age and the
// link mark.
//
// Todos with `lateness` (lib/focusRadar) also get count tabs that filter by
// lateness (All / overdue / today / due soon / waiting; empty ones hidden), and
// are ordered by it: the projects with the most late work first, the most late
// todo first inside.
// Any other sort in the ⇅ menu is one flat list, each row naming its project:
// todos by deadline (lib/todoDue compareByDue), notes by last edit or title.
// That flat list is the default (todos by deadline, notes by last edit); the
// choice is remembered in localStorage per kind.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  ArrowDownWideNarrow,
  Bot,
  CalendarDays,
  Check,
  ChevronDown,
  ChevronRight,
  Folder,
  Link2,
  Pin,
  PinOff,
  Plus,
  RotateCw,
  Search,
  Trash2,
  X,
} from 'lucide-vue-next'
import { AppSelect, Banner, Button, DropdownItem, DropdownMenu, DropdownSeparator, Input, SegmentedControl } from '@/components/ui'
import { relativeAge } from '@/lib/itemText'
import { formatDate, ymdToDate } from '@/lib/dateValue'
import { compareByDue, dueStatus, localYmd } from '@/lib/todoDue'
import {
  compareGroupLateness,
  compareLateness,
  groupLateness,
  type FocusRadarCounts,
  type FocusRadarItem,
  type FocusRadarStatus,
} from '@/lib/focusRadar'
import { formatFullDate } from '@/lib/slackThreadUtils'
import type { ItemRow } from './itemRow'
import { groupClass, groupHeaderClass, rowClass, rowListClass, rowSubClass, rowTitleClass } from '@/components/listDetail/rowStyles'

const props = defineProps<{
  kind: 'note' | 'todo'
  /** Already filtered. */
  rows: ItemRow[]
  /** True when there are no items at all (vs. none matching the filters). */
  emptyStore: boolean
  selectedId: string | null
  projectOptions: { value: string; label: string }[]
  /** project id → name, for the group headers. */
  projectNames: Record<string, string>
  error: string | null
  pinned: boolean
  reloading: boolean
  /** Todos: lateness of each open todo by id (done / unknown ones absent). */
  lateness?: Record<string, FocusRadarItem> | null
  /** Todos: counts behind the lateness tabs. */
  latenessCounts?: FocusRadarCounts | null
  /** Notes: counts behind the All / Linked tabs. */
  noteCounts?: { all: number; linked: number } | null
}>()

const search = defineModel<string>('search', { required: true })
const projectFilter = defineModel<string>('project', { required: true })
/** notes: '' | 'linked'; todos: 'active' | 'done' | 'all'. */
const filter = defineModel<string>('filter', { required: true })
/** Todos: show only one lateness status (null = all). */
const latenessFilter = defineModel<FocusRadarStatus | null>('latenessFilter', { default: null })

const LATENESS: FocusRadarStatus[] = ['overdue', 'today', 'due-soon', 'waiting']
/** The lateness tabs ('' = All); a tab is shown when it has todos or is picked. */
const LATENESS_TABS = computed(() => {
  const counts = props.latenessCounts
  const all = Object.values(counts ?? {}).reduce((sum, n) => sum + n, 0)
  return [
    { value: '', label: `${t('item.statusAll')} ${all}` },
    ...LATENESS.filter((s) => (counts?.[s] ?? 0) > 0 || latenessFilter.value === s).map((s) => ({
      value: s as string,
      label: `${t(`item.lateness.${s}`)} ${counts?.[s] ?? 0}`,
      title: t(`focus.radar.hint.${s}`, { days: 7 }),
    })),
  ]
})
const latenessTab = computed({
  get: () => latenessFilter.value ?? '',
  set: (v: string) => (latenessFilter.value = (v || null) as FocusRadarStatus | null),
})
/** Todos: done ones are behind the ⇅ menu ('all'); 'done' alone is legacy. */
const showDone = computed(() => filter.value !== 'active')

const emit = defineEmits<{
  select: [id: string]
  add: []
  toggleDone: [id: string]
  /** Delete every item of a folder: a project id, or null for "No project". */
  deleteGroup: [projectId: string | null, label: string]
  togglePin: []
  reload: []
  retry: []
  /** Todos: hand open todos to an AI conductor — a folder's (project id / null) or, with no arg, every visible one. */
  batch: [projectId?: string | null]
}>()

const { t, locale } = useI18n()

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

/** Notes: the All / Linked tabs (todos filter by lateness and "Show done"). */
const NOTE_TABS = computed(() => [
  { value: '', label: `${t('item.filterAll')} ${props.noteCounts?.all ?? 0}` },
  { value: 'linked', label: `${t('item.filterLinked')} ${props.noteCounts?.linked ?? 0}` },
])

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

const NONE = '__none__'

// ── Grouped by project, or one flat list by another sort ───────────────────
type SortMode = 'project' | 'due' | 'updated' | 'title'
const SORTS = computed<{ value: SortMode; label: string }[]>(() => [
  { value: 'project', label: t('item.sortByProject') },
  ...(props.kind === 'todo'
    ? [{ value: 'due' as const, label: t('item.sortByDue') }]
    : [
        { value: 'updated' as const, label: t('item.sortByUpdated') },
        { value: 'title' as const, label: t('item.sortByTitle') },
      ]),
])
const SORT_KEY = `items.${props.kind}.order`
const saved = localStorage.getItem(SORT_KEY)
/** Flat by default: todos by deadline, notes by last edit. */
const DEFAULT_SORT: SortMode = props.kind === 'todo' ? 'due' : 'updated'
const sortMode = ref<SortMode>(SORTS.value.find((o) => o.value === saved)?.value ?? DEFAULT_SORT)
watch(sortMode, (m) => {
  try {
    localStorage.setItem(SORT_KEY, m)
  } catch {
    /* storage unavailable */
  }
})
const flat = computed(() => sortMode.value !== 'project')
/** The ⇅ button is tinted when the list is not on its defaults. */
const customView = computed(() => sortMode.value !== DEFAULT_SORT || (props.kind === 'todo' && showDone.value))

function compareFlat(a: ItemRow, b: ItemRow): number {
  if (sortMode.value === 'due') return compareByDue(a, b)
  if (sortMode.value === 'updated') return a.at < b.at ? 1 : a.at > b.at ? -1 : 0
  // By title: untitled last.
  return (!a.title ? 1 : 0) - (!b.title ? 1 : 0) || a.title.localeCompare(b.title)
}
const flatRows = computed(() => [...props.rows].sort(compareFlat))

/**
 * Rows by project, "No project" last. Projects A–Z and list order inside a
 * group — or, for todos with lateness, the most late project / todo first.
 */
const groups = computed(() => {
  // Any other sort: a single header-less group.
  if (flat.value) return [{ key: '__flat__', label: '', rows: flatRows.value, late: groupLateness([]) }]
  const late = props.lateness
  const map = new Map<string, ItemRow[]>()
  for (const row of props.rows) {
    const key = row.projectId ?? NONE
    const list = map.get(key)
    if (list) list.push(row)
    else map.set(key, [row])
  }
  return [...map.entries()]
    .map(([key, rows]) => ({
      key,
      label: key === NONE ? t('item.noProject') : props.projectNames[key] ?? key,
      rows: late ? [...rows].sort((a, b) => compareLateness(late[a.id], late[b.id])) : rows,
      late: groupLateness(late ? rows.map((r) => late[r.id]) : []),
    }))
    .sort((a, b) =>
      (a.key === NONE ? 1 : 0) - (b.key === NONE ? 1 : 0) ||
      compareGroupLateness(a.late, b.late) ||
      a.label.localeCompare(b.label),
    )
})
</script>

<template>
  <aside class="list-sidebar flex w-[360px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <!-- Search + pin / reload, filter + new -->
    <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
      <div class="flex items-center gap-1.5">
        <div class="relative min-w-0 flex-1">
          <Search
            class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <Input v-model="search" class="pl-8 pr-7" :placeholder="t('item.searchPlaceholder')" />
          <button
            v-if="search"
            type="button"
            class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
            :title="t('item.clearSearch')"
            @click="search = ''"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>
        <Button
          :variant="pinned ? 'primary' : 'outline'"
          size="icon"
          class="shrink-0"
          :aria-pressed="pinned"
          :title="pinned ? t('item.unpin') : t('item.pin')"
          :aria-label="pinned ? t('item.unpin') : t('item.pin')"
          @click="emit('togglePin')"
        >
          <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          v-if="kind === 'todo'"
          variant="outline"
          size="icon"
          class="shrink-0"
          :title="t('item.batchAll')"
          :aria-label="t('item.batchAll')"
          @click="emit('batch')"
        >
          <Bot class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <!-- Reload from the DB (the MCP sidecar writes without events). -->
        <Button
          variant="outline"
          size="icon"
          class="shrink-0"
          :disabled="reloading"
          :title="t('item.reload')"
          :aria-label="t('item.reload')"
          @click="emit('reload')"
        >
          <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': reloading }" :stroke-width="1.75" />
        </Button>
      </div>
      <div class="flex items-center gap-1.5">
        <div class="min-w-0 flex-1">
          <AppSelect v-model="projectFilter" :options="projectOptions" size="sm">
            <template #leading>
              <Folder class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            </template>
          </AppSelect>
        </div>
        <!-- As wide as pin + reload above: the select lines up with the search box. -->
        <Button size="sm" class="w-[70px] shrink-0 px-0" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('common.new') }}
        </Button>
      </div>
      <div class="flex items-center gap-1.5">
        <SegmentedControl
          v-if="kind === 'note'"
          v-model="filter"
          :options="NOTE_TABS"
          class="min-w-0 flex-1"
          :aria-label="t('item.filterLinkedTitle')"
        />
        <SegmentedControl
          v-else-if="latenessCounts"
          v-model="latenessTab"
          :options="LATENESS_TABS"
          class="min-w-0 flex-1"
          :aria-label="t('item.filterStatusTitle')"
        />
        <div v-else class="flex-1" />
        <!-- Sort + view options; tinted when not on the defaults. -->
        <DropdownMenu align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-xs"
              class="shrink-0"
              :class="customView && 'border-primary/50 text-primary'"
              :title="t('item.viewOptions')"
              :aria-label="t('item.viewOptions')"
            >
              <ArrowDownWideNarrow class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <div class="px-2.5 pb-1 pt-1.5 text-[11px] font-medium text-muted-foreground">
            {{ t('item.sortTitle') }}
          </div>
          <DropdownItem v-for="o in SORTS" :key="o.value" @click="sortMode = o.value">
            <Check class="h-3.5 w-3.5" :class="sortMode === o.value ? '' : 'invisible'" :stroke-width="2" />
            {{ o.label }}
          </DropdownItem>
          <DropdownSeparator v-if="kind === 'todo'" />
          <DropdownItem v-if="kind === 'todo'" @click="filter = showDone ? 'active' : 'all'">
            <Check class="h-3.5 w-3.5" :class="showDone ? '' : 'invisible'" :stroke-width="2" />
            {{ t('item.showDone') }}
          </DropdownItem>
        </DropdownMenu>
      </div>
    </div>

    <!-- Groups -->
    <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <Banner v-if="error" tone="destructive" class="mb-2">
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
      <div v-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">
          {{ kind === 'note' ? t('item.emptyNote') : t('item.emptyTodo') }}
        </p>
        <p class="mt-1 text-[11px] text-muted-foreground/70">
          {{ kind === 'note' ? t('item.emptyNoteHint') : t('item.emptyTodoHint') }}
        </p>
      </div>
      <p v-else-if="rows.length === 0" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        {{ t('item.noResults') }}
      </p>

      <div v-for="g in groups" v-else :key="g.key" :class="groupClass">
        <div v-if="!flat" :class="groupHeaderClass">
          <button
            type="button"
            class="flex min-w-0 flex-1 cursor-pointer items-center gap-1 px-1.5 py-1 text-left text-xs font-semibold text-foreground"
            :aria-expanded="!collapsed.has(g.key)"
            @click="toggle(g.key)"
          >
            <component
              :is="collapsed.has(g.key) ? ChevronRight : ChevronDown"
              class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
              :stroke-width="2"
            />
            <Folder class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            <span class="min-w-0 flex-1 truncate">{{ g.label }}</span>
            <span
              v-if="g.late.overdue"
              class="shrink-0 rounded-full bg-destructive/15 px-1.5 text-[10px] font-medium tabular-nums text-destructive"
              :title="t('item.groupLate', { n: g.late.overdue, days: g.late.worstLateDays ?? 0 })"
            >
              {{ t('item.lateCount', { n: g.late.overdue }) }}
            </span>
            <span class="shrink-0 text-[10px] font-normal tabular-nums text-muted-foreground/80">
              {{ g.rows.length }}
            </span>
          </button>
          <Button
            v-if="kind === 'todo' && g.key !== NONE"
            variant="ghost"
            size="icon-xs"
            class="opacity-0 transition-opacity focus-visible:opacity-100 group-hover/folder:opacity-100"
            :title="t('item.batchFolder')"
            :aria-label="t('item.batchFolder')"
            @click="emit('batch', g.key)"
          >
            <Bot class="h-3 w-3" :stroke-width="1.75" />
          </Button>
          <Button
            variant="destructive-ghost"
            size="icon-xs"
            class="mr-1 opacity-0 transition-opacity focus-visible:opacity-100 group-hover/folder:opacity-100"
            :title="t('item.deleteFolder')"
            :aria-label="t('item.deleteFolder')"
            @click="emit('deleteGroup', g.key === NONE ? null : g.key, g.label)"
          >
            <Trash2 class="h-3 w-3" :stroke-width="1.75" />
          </Button>
        </div>

        <ul v-if="flat || !collapsed.has(g.key)" :class="rowListClass">
          <li v-for="row in g.rows" :key="row.id">
            <div
              class="gap-2 pr-2"
              :class="[flat ? 'pl-2' : 'pl-6', rowClass(row.id === selectedId)]"
            >
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
                @click="emit('select', row.id)"
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
                <!-- Flat list: the project leads the sub line, then a note's preview. -->
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
      </div>
    </div>
  </aside>
</template>
