<script setup lang="ts" generic="T extends { id: string }">
// Left pane of the list-detail screens (Servers, Rules, Skills, MCP), built
// from the same pieces as the Todos / Notes ItemSidebar:
//
//   Skills 12                                     [+ New] [⋯]
//   ( 🔍 Search skills…  (⌘F)                          ⚙︎• )
//   Filtering: Design                          Clear filters   ← only while filtering
//   ─────────────────────────────────────────────────────────
//   DESIGN                                                  5
//   row …
//
// The search box's one menu holds the host's filter sections (`filterDefs`,
// one radio choice each, "All" first; bound through `v-model:filters`), the
// host's sorts, "Group by …" and "Reset to defaults". ⋯ holds Reload, the
// host's secondary actions (`#menu`) and Hide the list. `busy` spins a small
// icon in the header while such an action runs.
//
// Sort and folders are remembered in localStorage under `${collapseKey}.view`
// (lib/listSidebarView), the collapsed folders under `collapseKey`; filters
// are not remembered. Ungrouped, each row names its folder.
//
// The host filters and groups the rows itself (see lib/listGroups.ts) and
// renders each row's content through the `row` slot; the sidebar owns the
// row's selection chrome and the folder headers.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Folder, RefreshCw, RotateCw } from 'lucide-vue-next'
import { Banner, Button, DropdownItem, DropdownSeparator } from '@/components/ui'
import { parseCollapsed, type ListSort, type RowGroup } from '@/lib/listGroups'
import {
  clearListFilters,
  listFilterSummary,
  listViewDiffers,
  parseListDisplay,
  pruneListFilters,
  type ListDisplay,
  type ListFilterDef,
  type ListFilterValues,
} from '@/lib/listSidebarView'
import { useListPaneSidebar } from '@/composables/useListPane'
import ListSidebarHeader from './ListSidebarHeader.vue'
import ListSearchBox from './ListSearchBox.vue'
import ListFilterSummary from './ListFilterSummary.vue'
import ListGroupHeader from './ListGroupHeader.vue'
import ListMenuLabel from './ListMenuLabel.vue'
import ListMenuCheck from './ListMenuCheck.vue'
import { rowClass, rowListClass, rowSubClass } from './rowStyles'

const props = withDefaults(
  defineProps<{
    title: string
    /** Already filtered + grouped. */
    groups: RowGroup<T>[]
    selectedId: string | null
    /** localStorage key of the collapsed-group set. */
    collapseKey: string
    /** Tooltip of "+ New" (e.g. with a shortcut). */
    newTitle?: string
    searchPlaceholder?: string
    /** Filter sections of the search menu; their choices go to `v-model:filters`. */
    filterDefs?: ListFilterDef[]
    loading?: boolean
    error?: string | null
    /** True when there are no items at all (vs. none matching the filters). */
    emptyStore?: boolean
    emptyTitle?: string
    emptyHint?: string
    /** Hide the folder headers when there is a single group. */
    flatWhenSingle?: boolean
    /** Sorts of the menu (none → no Sort section; rows keep the host's order). */
    sorts?: ListSort<T>[]
    /** The sort until the user picks one; defaults to the first. */
    defaultSort?: string
    /** Label of the folders toggle; defaults to "Group by group". */
    groupedLabel?: string
    /** Folders until the user toggles them. */
    defaultGrouped?: boolean
    /** Tooltip of a spinner in the header while a list action (import, test all…) runs. */
    busy?: string | null
  }>(),
  {
    newTitle: undefined,
    searchPlaceholder: undefined,
    filterDefs: () => [],
    loading: false,
    error: null,
    emptyStore: false,
    emptyTitle: '',
    emptyHint: '',
    flatWhenSingle: true,
    sorts: () => [],
    defaultSort: '',
    groupedLabel: '',
    defaultGrouped: false,
    busy: null,
  },
)

const search = defineModel<string>('search', { required: true })
const filters = defineModel<ListFilterValues>('filters', { default: () => ({}) })

// Hideable per screen (⌘B) when the screen provides a list pane.
const rootEl = ref<HTMLElement | null>(null)
const listPane = useListPaneSidebar(rootEl)

const emit = defineEmits<{
  select: [id: string]
  add: []
  /** ⋯ Reload, and the error banner's retry. */
  reload: []
}>()

defineSlots<{
  /** Secondary actions of the ⋯ menu (DropdownItem / DropdownSeparator), after Reload. */
  menu?: () => unknown
  /** A row's content (inside the clickable, selectable area). */
  row: (p: { row: T; selected: boolean }) => unknown
  /** Hover actions at the end of a row (outside the click area). */
  'row-actions'?: (p: { row: T }) => unknown
  /** Hover actions on a folder header (Button size="icon-xs"). */
  'group-actions'?: (p: { group: RowGroup<T> }) => unknown
}>()

const { t } = useI18n()

// ── Collapsed folders ──────────────────────────────────────────────────────

const collapsed = ref<Set<string>>(parseCollapsed(readStorage(props.collapseKey)))

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function writeStorage(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch {
    /* storage unavailable */
  }
}

function toggle(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
  writeStorage(props.collapseKey, JSON.stringify([...next]))
}

// ── View (the search box's menu) ───────────────────────────────────────────

const defaults = computed<ListDisplay>(() => ({
  sort: props.defaultSort || props.sorts[0]?.value || '',
  grouped: props.defaultGrouped,
}))
const VIEW_KEY = `${props.collapseKey}.view`
const display = ref<ListDisplay>(
  parseListDisplay(
    readStorage(VIEW_KEY),
    // The older "order" setting: '' = folders, else a flat sort.
    readStorage(`${props.collapseKey}.order`),
    props.sorts.map((s) => s.value),
    defaults.value,
  ),
)
watch(display, (d) => writeStorage(VIEW_KEY, JSON.stringify(d)))

function patchDisplay(p: Partial<ListDisplay>) {
  display.value = { ...display.value, ...p }
}

// A choice no longer offered (deleted group, tag gone…) shows everything again.
watch(
  () => props.filterDefs,
  (defs) => {
    const next = pruneListFilters(defs, filters.value)
    if (next) filters.value = next
  },
  { immediate: true },
)

function setFilter(key: string, value: string) {
  filters.value = { ...filters.value, [key]: value }
}

const summary = computed(() => listFilterSummary(props.filterDefs, filters.value))
const changed = computed(() => listViewDiffers(filters.value, display.value, defaults.value))

function clearFilters() {
  search.value = ''
  filters.value = clearListFilters(filters.value)
}
function resetView() {
  filters.value = clearListFilters(filters.value)
  display.value = { ...defaults.value }
}

// ── Rows ───────────────────────────────────────────────────────────────────

const activeSort = computed(() => props.sorts.find((o) => o.value === display.value.sort) ?? null)
const flat = computed(() => !display.value.grouped)

/** Every row once (a row can sit in several groups), with its first group's label. */
const uniqueRows = computed(() => {
  const seen = new Map<string, { row: T; group: string }>()
  for (const g of props.groups) for (const row of g.rows) if (!seen.has(row.id)) seen.set(row.id, { row, group: g.label })
  return [...seen.values()]
})
const groupOf = computed(() => new Map(uniqueRows.value.map((x) => [x.row.id, x.group])))

const shownGroups = computed<RowGroup<T>[]>(() => {
  const sort = activeSort.value
  if (props.groups.length === 0) return []
  if (flat.value) {
    const rows = uniqueRows.value.map((x) => x.row)
    return [{ key: '__flat__', label: '', rows: sort ? rows.sort(sort.compare) : rows }]
  }
  return sort ? props.groups.map((g) => ({ ...g, rows: [...g.rows].sort(sort.compare) })) : props.groups
})

const showHeaders = computed(() => !flat.value && !(props.flatWhenSingle && props.groups.length === 1))
/** Flat list of several folders: each row names its own. */
const showRowGroup = computed(() => flat.value && props.groups.length > 1)
</script>

<template>
  <aside
    v-if="listPane.shown.value"
    ref="rootEl"
    class="list-sidebar flex shrink-0 flex-col border-r border-border/60 bg-sidebar"
    :class="[listPane.widthClass.value, listPane.floatClass.value]"
  >
    <ListSidebarHeader
      :title="title"
      :count="loading && uniqueRows.length === 0 ? null : uniqueRows.length"
      :new-label="t('common.new')"
      :new-title="newTitle"
      @new="emit('add')"
    >
      <template v-if="busy" #actions>
        <span class="flex h-8 w-5 shrink-0 items-center justify-center text-muted-foreground" :title="busy" role="status" :aria-label="busy">
          <RefreshCw class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        </span>
      </template>
      <template #menu>
        <DropdownItem :disabled="loading" @click="emit('reload')">
          <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loading }" :stroke-width="1.75" />
          {{ t('listPane.reload') }}
        </DropdownItem>
        <template v-if="$slots.menu">
          <DropdownSeparator />
          <slot name="menu" />
        </template>
      </template>
    </ListSidebarHeader>

    <div class="shrink-0 px-2.5 pb-1.5 pt-0.5">
      <ListSearchBox v-model="search" :placeholder="searchPlaceholder" :filter-active="changed">
        <template #filters>
          <template v-for="def in filterDefs" :key="def.key">
            <ListMenuLabel>{{ def.title }}</ListMenuLabel>
            <ListMenuCheck radio :checked="!filters[def.key]" @click="setFilter(def.key, '')">
              {{ t('listPane.all') }}
            </ListMenuCheck>
            <ListMenuCheck
              v-for="o in def.options"
              :key="o.value"
              radio
              :checked="filters[def.key] === o.value"
              :count="o.count"
              @click="setFilter(def.key, o.value)"
            >
              {{ o.label }}
            </ListMenuCheck>
            <DropdownSeparator />
          </template>
          <template v-if="sorts.length > 0">
            <ListMenuLabel>{{ t('common.sort') }}</ListMenuLabel>
            <ListMenuCheck
              v-for="s in sorts"
              :key="s.value"
              radio
              :checked="display.sort === s.value"
              @click="patchDisplay({ sort: s.value })"
            >
              {{ s.label }}
            </ListMenuCheck>
            <DropdownSeparator />
          </template>
          <ListMenuLabel>{{ t('listPane.display') }}</ListMenuLabel>
          <ListMenuCheck :checked="display.grouped" @click="patchDisplay({ grouped: !display.grouped })">
            {{ groupedLabel || t('listPane.groupByGroup') }}
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

    <div class="min-h-0 flex-1 overflow-y-auto border-t border-border/40 px-2 pb-2">
      <Banner v-if="error" tone="destructive" class="mb-2 mt-2">
        {{ error }}
        <template #actions>
          <Button
            variant="ghost"
            size="icon-xs"
            :title="t('listPane.reload')"
            :aria-label="t('listPane.reload')"
            @click="emit('reload')"
          >
            <RotateCw class="h-3 w-3" :stroke-width="2" />
          </Button>
        </template>
      </Banner>

      <div v-if="loading && shownGroups.length === 0" class="mt-1.5 space-y-1 px-1">
        <div v-for="i in 5" :key="i" class="h-10 animate-pulse rounded-md bg-muted/60" />
      </div>
      <div v-else-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">{{ emptyTitle }}</p>
        <p v-if="emptyHint" class="mt-1 text-[11px] text-muted-foreground/70">{{ emptyHint }}</p>
      </div>
      <div v-else-if="shownGroups.length === 0 && !error" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        <p>{{ t('common.noResults') }}</p>
        <button
          v-if="search || summary.length"
          type="button"
          class="mt-1 cursor-pointer underline underline-offset-2 hover:text-foreground"
          @click="clearFilters"
        >
          {{ t('listPane.clearFilters') }}
        </button>
      </div>

      <template v-for="g in shownGroups" v-else :key="g.key">
        <ListGroupHeader
          v-if="showHeaders"
          :label="g.label"
          :count="g.rows.length"
          :collapsed="collapsed.has(g.key)"
          @toggle="toggle(g.key)"
        >
          <template v-if="$slots['group-actions']" #actions>
            <div class="flex shrink-0 items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover/folder:opacity-100">
              <slot name="group-actions" :group="g" />
            </div>
          </template>
        </ListGroupHeader>

        <ul v-if="!showHeaders || !collapsed.has(g.key)" :class="[rowListClass, showHeaders ? 'mb-2' : 'mt-1.5']">
          <li v-for="row in g.rows" :key="row.id">
            <div class="group/row gap-1 pl-2.5 pr-2" :class="rowClass(row.id === selectedId)">
              <button
                type="button"
                class="flex min-w-0 flex-1 cursor-pointer flex-col text-left"
                @click="emit('select', row.id); listPane.picked()"
              >
                <slot name="row" :row="row" :selected="row.id === selectedId" />
                <!-- Flat list: the row's folder. -->
                <span v-if="showRowGroup" class="flex w-full min-w-0 items-center gap-1" :class="rowSubClass">
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
      </template>
    </div>
  </aside>
</template>
