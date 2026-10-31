<script setup lang="ts" generic="T extends { id: string }">
// Left pane of the list-detail screens (Servers, Rules, Skills, MCP) — the
// same layout as the Notes window's ItemSidebar: search (+ toolbar slot for
// icon buttons), an optional controls row next to "+ New", an optional
// segmented filter, then the rows in collapsible groups (remembered in
// localStorage under `collapseKey`).
//
// The host filters and groups the rows itself (see lib/listGroups.ts) and
// renders each row's content through the `row` slot; the sidebar owns the
// row's selection chrome and the folder headers.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, ChevronDown, ChevronRight, Folder, Plus, RotateCw, Search, X } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { parseCollapsed, type RowGroup } from '@/lib/listGroups'

const props = withDefaults(
  defineProps<{
    /** Already filtered + grouped. */
    groups: RowGroup<T>[]
    selectedId: string | null
    /** localStorage key of the collapsed-group set. */
    collapseKey: string
    newLabel: string
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
  }>(),
  {
    searchPlaceholder: '',
    filters: () => [],
    loading: false,
    error: null,
    emptyStore: false,
    emptyTitle: '',
    emptyHint: '',
    flatWhenSingle: true,
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

function showHeaders(): boolean {
  return !(props.flatWhenSingle && props.groups.length === 1)
}
</script>

<template>
  <aside class="flex w-[280px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
      <div class="flex items-center gap-1.5">
        <div class="relative min-w-0 flex-1">
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
        <slot name="toolbar" />
      </div>
      <div class="flex items-center gap-1.5">
        <div class="min-w-0 flex-1">
          <slot name="controls" />
        </div>
        <Button size="sm" class="shrink-0" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ newLabel }}
        </Button>
      </div>
      <div
        v-if="filters.length > 0"
        class="flex rounded-md bg-muted p-0.5 text-[11px] font-medium"
        role="group"
      >
        <button
          v-for="f in filters"
          :key="f.value"
          type="button"
          class="flex flex-1 cursor-pointer items-center justify-center rounded px-2 py-1 transition-colors"
          :class="filter === f.value
            ? 'bg-background text-foreground shadow-sm'
            : 'text-muted-foreground hover:text-foreground'"
          :aria-pressed="filter === f.value"
          @click="filter = f.value"
        >
          {{ f.label }}
        </button>
      </div>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <div
        v-if="error"
        class="mb-2 flex items-start gap-1.5 rounded-md border border-destructive/30 bg-destructive/10 px-2 py-1.5 text-[11px] text-foreground"
      >
        <AlertCircle class="mt-px h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="2" />
        <span class="min-w-0 flex-1 break-words">{{ error }}</span>
        <button
          type="button"
          class="shrink-0 cursor-pointer rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          :title="t('common.refresh')"
          :aria-label="t('common.refresh')"
          @click="emit('retry')"
        >
          <RotateCw class="h-3 w-3" :stroke-width="2" />
        </button>
      </div>

      <div v-if="loading && groups.length === 0" class="space-y-1 px-1">
        <div v-for="i in 5" :key="i" class="h-10 animate-pulse rounded-md bg-muted/60" />
      </div>
      <div v-else-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">{{ emptyTitle }}</p>
        <p v-if="emptyHint" class="mt-1 text-[11px] text-muted-foreground/70">{{ emptyHint }}</p>
      </div>
      <p v-else-if="groups.length === 0 && !error" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        {{ t('common.noResults') }}
      </p>

      <div v-for="g in groups" v-else :key="g.key" class="mb-2">
        <div v-if="showHeaders()" class="group/folder flex items-center rounded hover:bg-accent/50">
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

        <ul v-if="!showHeaders() || !collapsed.has(g.key)" class="mt-px space-y-px">
          <li v-for="row in g.rows" :key="row.id">
            <div
              class="group/row flex w-full items-start gap-1 rounded-md py-1.5 pr-1.5 transition-colors"
              :class="[
                showHeaders() ? 'pl-6' : 'pl-2',
                row.id === selectedId ? 'bg-primary/15 text-foreground' : 'text-foreground/85 hover:bg-accent/50',
              ]"
            >
              <button
                type="button"
                class="flex min-w-0 flex-1 cursor-pointer flex-col text-left"
                @click="emit('select', row.id)"
              >
                <slot name="row" :row="row" :selected="row.id === selectedId" />
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
