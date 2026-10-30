<script setup lang="ts">
// Left pane of the Notes / Todos window — the sibling of SlackSidebar, same
// layout: search + the window's pin / reload (no custom titlebar), project
// filter + "+ New", a segmented filter (notes: All | Linked to a session;
// todos: Open | Done | All), then the items grouped by project (each header
// collapsible, remembered in localStorage per kind).
//
// A note row: title, a one-line preview, its age and a link mark with the
// linked-session count. A todo row: a clickable checkbox, its first line, its
// deadline (red when overdue, amber when due within a few days), age and the
// link mark.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertCircle,
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
  X,
} from 'lucide-vue-next'
import { AppSelect, Button, Input } from '@/components/ui'
import { relativeAge } from '@/lib/itemText'
import { formatDate, ymdToDate } from '@/lib/dateValue'
import { dueStatus, localYmd } from '@/lib/todoDue'
import { formatFullDate } from '@/lib/slackThreadUtils'
import type { ItemRow } from './itemRow'

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
}>()

const search = defineModel<string>('search', { required: true })
const projectFilter = defineModel<string>('project', { required: true })
/** notes: '' | 'linked'; todos: 'active' | 'done' | 'all'. */
const filter = defineModel<string>('filter', { required: true })

const emit = defineEmits<{
  select: [id: string]
  add: []
  toggleDone: [id: string]
  togglePin: []
  reload: []
  retry: []
}>()

const { t, locale } = useI18n()

/** Deadline badges by row id: short date + urgency (a done todo is never late). */
const dueBadges = computed(() => {
  const today = localYmd()
  const out: Record<string, { label: string; title: string; tone: string }> = {}
  for (const row of props.rows) {
    const d = ymdToDate(row.dueDate)
    if (!d) continue
    const status = row.done ? 'later' : dueStatus(row.dueDate, today)
    out[row.id] = {
      label: formatDate(d, locale.value, {
        day: 'numeric',
        month: 'short',
        ...(row.dueDate!.slice(0, 4) === today.slice(0, 4) ? {} : { year: 'numeric' }),
      }),
      title: t('item.dueOn', { date: formatDate(d, locale.value, { dateStyle: 'full' }) }),
      tone:
        status === 'overdue' ? 'bg-destructive/15 font-medium text-destructive'
        : status === 'today' || status === 'soon' ? 'bg-amber-500/15 font-medium text-amber-600 dark:text-amber-400'
        : 'text-muted-foreground',
    }
  }
  return out
})

const FILTERS = computed(() =>
  props.kind === 'note'
    ? [
        { value: '', label: t('item.filterAll') },
        { value: 'linked', label: t('item.filterLinked') },
      ]
    : [
        { value: 'active', label: t('item.statusActive') },
        { value: 'done', label: t('item.statusDone') },
        { value: 'all', label: t('item.statusAll') },
      ],
)

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

/** Rows by project (A–Z), "No project" last; list order kept inside a group. */
const groups = computed(() => {
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
      rows,
    }))
    .sort((a, b) => (a.key === NONE ? 1 : b.key === NONE ? -1 : a.label.localeCompare(b.label)))
})
</script>

<template>
  <aside class="flex w-[280px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
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
          size="icon-sm"
          class="shrink-0"
          :aria-pressed="pinned"
          :title="pinned ? t('item.unpin') : t('item.pin')"
          :aria-label="pinned ? t('item.unpin') : t('item.pin')"
          @click="emit('togglePin')"
        >
          <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <!-- Reload from the DB (the MCP sidecar writes without events). -->
        <Button
          variant="outline"
          size="icon-sm"
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
        <Button size="sm" class="shrink-0" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ kind === 'note' ? t('item.newNote') : t('item.newTodo') }}
        </Button>
      </div>
      <div
        class="flex rounded-md bg-muted p-0.5 text-[11px] font-medium"
        role="group"
        :aria-label="kind === 'note' ? t('item.filterLinkedTitle') : t('item.filterStatusTitle')"
      >
        <button
          v-for="f in FILTERS"
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

    <!-- Groups -->
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
          :title="t('item.reload')"
          :aria-label="t('item.reload')"
          @click="emit('retry')"
        >
          <RotateCw class="h-3 w-3" :stroke-width="2" />
        </button>
      </div>
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

      <div v-for="g in groups" v-else :key="g.key" class="mb-2">
        <button
          type="button"
          class="flex w-full cursor-pointer items-center gap-1 rounded px-1.5 py-1 text-left text-xs font-semibold text-foreground hover:bg-accent/50"
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

        <ul v-if="!collapsed.has(g.key)" class="mt-px space-y-px">
          <li v-for="row in g.rows" :key="row.id">
            <div
              class="flex w-full items-start gap-2 rounded-md py-1.5 pl-6 pr-2 transition-colors"
              :class="row.id === selectedId
                ? 'bg-primary/15 text-foreground'
                : 'text-foreground/85 hover:bg-accent/50'"
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
                    class="min-w-0 flex-1 truncate text-[13px]"
                    :class="row.done && 'text-muted-foreground line-through'"
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
                <span v-if="row.preview" class="mt-0.5 block w-full truncate text-[11px] text-muted-foreground">
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
