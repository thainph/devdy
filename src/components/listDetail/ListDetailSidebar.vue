<script setup lang="ts" generic="T extends { id: string }">
// Left pane of the list-detail screens (Servers, Rules, Skills, MCP) — the
// same layout as the Notes window's ItemSidebar: search (+ toolbar slot for
// icon buttons), the controls slot next to "+ New" (one grid, so the controls
// line up with the search box whatever the toolbar holds), the segmented
// filter — or the row count — ending in a ⇅ sort menu when the host gives
// `sorts`, then the rows in collapsible groups (remembered in localStorage
// under `collapseKey`). A host sort shows one flat list instead, each row
// naming its group; `defaultSort` picks the first one, the user's choice is
// remembered under `${collapseKey}.order`.
//
// The host filters and groups the rows itself (see lib/listGroups.ts) and
// renders each row's content through the `row` slot; the sidebar owns the
// row's selection chrome and the folder headers.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowDownWideNarrow, Check, ChevronDown, ChevronRight, Folder, Plus, RotateCw, Search, X } from 'lucide-vue-next'
import { Banner, Button, DropdownItem, DropdownMenu, Input, SegmentedControl } from '@/components/ui'
import { parseCollapsed, type ListSort, type RowGroup } from '@/lib/listGroups'
import { groupClass, groupHeaderClass, rowClass, rowListClass, rowSubClass } from './rowStyles'

const props = withDefaults(
  defineProps<{
    /** Already filtered + grouped. */
    groups: RowGroup<T>[]
    selectedId: string | null
    /** localStorage key of the collapsed-group set. */
    collapseKey: string
    /** Defaults to "New" — every list-detail screen uses the same label. */
    newLabel?: string
    searchPlaceholder?: string
    /** Segmented filter options; none → no filter row. */
    filters?: { value: string; label: string }[]
    loading?: boolean
    error?: string | null
    /** True when there are no items at all (vs. none matching the filters). */
    emptyStore?: boolean
    emptyTitle?: string
    emptyHint?: string
    /** Hide the folder headers when there is a single group. */
    flatWhenSingle?: boolean
    /** Flat-list sorts (none → no ⇅ menu). */
    sorts?: ListSort<T>[]
    /** Label of the grouped order; defaults to "By group". */
    groupedLabel?: string
    /** The sort until the user picks one: a `sorts` value, or '' = grouped. */
    defaultSort?: string
  }>(),
  {
    newLabel: '',
    searchPlaceholder: '',
    filters: () => [],
    loading: false,
    error: null,
    emptyStore: false,
    emptyTitle: '',
    emptyHint: '',
    flatWhenSingle: true,
    sorts: () => [],
    groupedLabel: '',
    defaultSort: '',
  },
)

const search = defineModel<string>('search', { required: true })
const filter = defineModel<string>('filter', { default: '' })

const emit = defineEmits<{
  select: [id: string]
  add: []
  retry: []
}>()

defineSlots<{
  /** Icon buttons next to the search box. */
  toolbar?: () => unknown
  /** Left of "+ New" (e.g. a group/tag select). */
  controls?: () => unknown
  /** A row's content (inside the clickable, selectable area). */
  row: (p: { row: T; selected: boolean }) => unknown
  /** Hover actions at the end of a row (outside the click area). */
  'row-actions'?: (p: { row: T }) => unknown
  /** Hover actions on a folder header. */
  'group-actions'?: (p: { group: RowGroup<T> }) => unknown
}>()

const { t } = useI18n()

const collapsed = ref<Set<string>>(parseCollapsed(readStorage()))

function readStorage(): string | null {
  try {
    return localStorage.getItem(props.collapseKey)
  } catch {
    return null
  }
}

function toggle(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
  try {
    localStorage.setItem(props.collapseKey, JSON.stringify([...next]))
  } catch {
    /* storage unavailable */
  }
}

// ── Grouped, or one flat list by a host sort ───────────────────────────────
const SORT_KEY = `${props.collapseKey}.order`
const GROUPED = ''
const sortMode = ref<string>(readSort())
function readSort(): string {
  try {
    const saved = localStorage.getItem(SORT_KEY)
    if (saved === GROUPED || props.sorts.some((o) => o.value === saved)) return saved!
  } catch {
    /* storage unavailable */
  }
  return props.defaultSort
}
watch(sortMode, (m) => {
  try {
    localStorage.setItem(SORT_KEY, m)
  } catch {
    /* storage unavailable */
  }
})
const sortOptions = computed(() => [
  { value: GROUPED, label: props.groupedLabel || t('common.sortByGroup') },
  ...props.sorts.map((o) => ({ value: o.value, label: o.label })),
])
const activeSort = computed(() => props.sorts.find((o) => o.value === sortMode.value) ?? null)

/** Every row once (a row can sit in several groups), with its first group's label. */
const uniqueRows = computed(() => {
  const seen = new Map<string, { row: T; group: string }>()
  for (const g of props.groups) for (const row of g.rows) if (!seen.has(row.id)) seen.set(row.id, { row, group: g.label })
  return [...seen.values()]
})
const groupOf = computed(() => new Map(uniqueRows.value.map((x) => [x.row.id, x.group])))

const shownGroups = computed<RowGroup<T>[]>(() => {
  const sort = activeSort.value
  if (!sort || props.groups.length === 0) return props.groups
  const rows = uniqueRows.value.map((x) => x.row).sort(sort.compare)
  return [{ key: '__flat__', label: '', rows }]
})

function showHeaders(): boolean {
  if (activeSort.value) return false
  return !(props.flatWhenSingle && props.groups.length === 1)
}
</script>

<template>
  <aside class="list-sidebar flex w-[360px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
      <!-- One grid: the controls line up with the search box, "+ New" with the toolbar. -->
      <div class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-1.5">
        <div class="relative min-w-0">
          <Search
            class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <Input v-model="search" class="pl-8 pr-7" :placeholder="searchPlaceholder || t('common.search')" />
          <button
            v-if="search"
            type="button"
            class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
            :title="t('common.clear')"
            @click="search = ''"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>
        <div class="flex items-center justify-end gap-1.5">
          <slot name="toolbar" />
        </div>
        <div class="min-w-0">
          <slot name="controls" />
        </div>
        <Button size="sm" class="min-w-[70px]" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ newLabel || t('common.new') }}
        </Button>
      </div>
      <div v-if="filters.length > 0 || sorts.length > 0" class="flex items-center gap-1.5">
        <SegmentedControl v-if="filters.length > 0" v-model="filter" :options="filters" class="min-w-0 flex-1" />
        <span v-else class="min-w-0 flex-1 truncate px-1 text-[11px] text-muted-foreground">
          {{ t('common.itemCount', { n: uniqueRows.length }, uniqueRows.length) }}
        </span>
        <!-- Sort; tinted when not on the default. -->
        <DropdownMenu v-if="sorts.length > 0" align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-xs"
              class="shrink-0"
              :class="sortMode !== defaultSort && 'border-primary/50 text-primary'"
              :title="t('common.sort')"
              :aria-label="t('common.sort')"
            >
              <ArrowDownWideNarrow class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <div class="px-2.5 pb-1 pt-1.5 text-[11px] font-medium text-muted-foreground">
            {{ t('common.sort') }}
          </div>
          <DropdownItem v-for="o in sortOptions" :key="o.value" @click="sortMode = o.value">
            <Check class="h-3.5 w-3.5" :class="sortMode === o.value ? '' : 'invisible'" :stroke-width="2" />
            {{ o.label }}
          </DropdownItem>
        </DropdownMenu>
      </div>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <Banner v-if="error" tone="destructive" class="mb-2">
        {{ error }}
        <template #actions>
          <Button
            variant="ghost"
            size="icon-xs"
            :title="t('common.refresh')"
            :aria-label="t('common.refresh')"
            @click="emit('retry')"
          >
            <RotateCw class="h-3 w-3" :stroke-width="2" />
          </Button>
        </template>
      </Banner>

      <div v-if="loading && shownGroups.length === 0" class="space-y-1 px-1">
        <div v-for="i in 5" :key="i" class="h-10 animate-pulse rounded-md bg-muted/60" />
      </div>
      <div v-else-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">{{ emptyTitle }}</p>
        <p v-if="emptyHint" class="mt-1 text-[11px] text-muted-foreground/70">{{ emptyHint }}</p>
      </div>
      <p v-else-if="shownGroups.length === 0 && !error" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        {{ t('common.noResults') }}
      </p>

      <div v-for="g in shownGroups" v-else :key="g.key" :class="groupClass">
        <div v-if="showHeaders()" :class="groupHeaderClass">
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
            <span class="shrink-0 text-[10px] font-normal tabular-nums text-muted-foreground/80">
              {{ g.rows.length }}
            </span>
          </button>
          <div class="mr-1 flex shrink-0 items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover/folder:opacity-100">
            <slot name="group-actions" :group="g" />
          </div>
        </div>

        <ul v-if="!showHeaders() || !collapsed.has(g.key)" :class="rowListClass">
          <li v-for="row in g.rows" :key="row.id">
            <div
              class="group/row gap-1 pr-2"
              :class="[rowClass(row.id === selectedId), showHeaders() ? 'pl-6' : 'pl-3']"
            >
              <button
                type="button"
                class="flex min-w-0 flex-1 cursor-pointer flex-col text-left"
                @click="emit('select', row.id)"
              >
                <slot name="row" :row="row" :selected="row.id === selectedId" />
                <!-- Flat list: the row's group. -->
                <span v-if="activeSort" class="flex w-full min-w-0 items-center gap-1" :class="rowSubClass">
                  <Folder class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                  <span class="truncate">{{ groupOf.get(row.id) }}</span>
                </span>
              </button>
              <div
                v-if="$slots['row-actions']"
                class="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/row:opacity-100"
              >
                <slot name="row-actions" :row="row" />
              </div>
            </div>
          </li>
        </ul>
      </div>
    </div>
  </aside>
</template>
