<script setup lang="ts">
// Left pane of the Slack threads window, laid out like Slack's sidebar: search
// + the window's pin / reload (the window has no custom titlebar), project
// filter + "Add thread" (as wide as pin + reload, so the select lines up with
// the search box), All | Linked | Not linked session tabs with counts ending in
// a ⇅ sort menu, then a workspace → #channel tree (each header collapsible,
// remembered in localStorage) of thread rows. Sorting by recent activity or
// title instead gives one flat list, each row naming its workspace / channel;
// recent activity is the default, the sort is remembered in localStorage. A row shows the title, the last reply's age
// (full date in the tooltip), a new-message badge, a status mark (spinner /
// error / warning), a link mark with the linked-session count and an "Old"
// badge for legacy rows.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  ArrowDownWideNarrow,
  Check,
  ChevronDown,
  ChevronRight,
  Folder,
  Hash,
  Link2,
  Pin,
  PinOff,
  Plus,
  RotateCw,
  Search,
  X,
} from 'lucide-vue-next'
import { AppSelect, Banner, Button, DropdownItem, DropdownMenu, Input, SegmentedControl } from '@/components/ui'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate, groupThreads } from '@/lib/slackThreadUtils'
import type { SlackThreadProgress, SlackThreadSummary } from '@/stores/slackThreads'
import { rowClass, rowListClass, rowSubClass, rowTitleClass } from '@/components/listDetail/rowStyles'
import SlackStatusIcon from './SlackStatusIcon.vue'

const props = defineProps<{
  /** Already filtered by project / search. */
  threads: SlackThreadSummary[]
  /** True when there are no threads at all (vs. none matching the filters). */
  emptyStore: boolean
  selectedId: string | null
  progress: Record<string, SlackThreadProgress>
  projectOptions: { value: string; label: string }[]
  /** The list failed to load (shown above the tree with a retry). */
  error: string | null
  /** Window pinned above other apps (useFloatingWindow). */
  pinned: boolean
  /** A full reload is in flight. */
  reloading: boolean
  /** Counts behind the link tabs: the project in scope, before the link filter. */
  linkCounts?: { all: number; linked: number; unlinked: number } | null
}>()

const search = defineModel<string>('search', { required: true })
const projectFilter = defineModel<string>('project', { required: true })
/** '' = all, else only threads linked / not linked to a session. */
const linkFilter = defineModel<'' | 'linked' | 'unlinked'>('link', { required: true })

const LINK_FILTERS = [
  { value: '', label: 'slackThread.sidebar.linkAll', count: 'all' },
  { value: 'linked', label: 'slackThread.sidebar.linked', count: 'linked' },
  { value: 'unlinked', label: 'slackThread.sidebar.unlinked', count: 'unlinked' },
] as const

const emit = defineEmits<{
  select: [id: string]
  add: []
  retry: []
  togglePin: []
  reload: []
}>()

const { t } = useI18n()
const linkFilterOptions = computed(() =>
  LINK_FILTERS.map((f) => ({
    value: f.value,
    label: props.linkCounts ? `${t(f.label)} ${props.linkCounts[f.count]}` : t(f.label),
  })),
)

// ── The workspace → channel tree, or one flat list by another sort ─────────
type SortMode = 'channel' | 'recent' | 'title'
const SORT_KEY = 'slackThreads.order'
const SORTS = computed<{ value: SortMode; label: string }[]>(() => [
  { value: 'channel', label: t('slackThread.sidebar.sortByChannel') },
  { value: 'recent', label: t('slackThread.sidebar.sortByRecent') },
  { value: 'title', label: t('slackThread.sidebar.sortByTitle') },
])
const saved = localStorage.getItem(SORT_KEY)
/** Flat by default: the latest activity first. */
const DEFAULT_SORT: SortMode = 'recent'
const sortMode = ref<SortMode>(SORTS.value.find((o) => o.value === saved)?.value ?? DEFAULT_SORT)
watch(sortMode, (m) => {
  try {
    localStorage.setItem(SORT_KEY, m)
  } catch {
    /* storage unavailable */
  }
})
const flat = computed(() => sortMode.value !== 'channel')

const flatThreads = computed(() =>
  [...props.threads].sort((a, b) => {
    if (sortMode.value === 'recent') {
      const x = a.lastReplyAt ?? a.updatedAt
      const y = b.lastReplyAt ?? b.updatedAt
      return x < y ? 1 : x > y ? -1 : 0
    }
    // By title: untitled last.
    return (!a.title ? 1 : 0) - (!b.title ? 1 : 0) || a.title.localeCompare(b.title)
  }),
)

/** "workspace · #channel" under a flat-list row. */
function rowPlace(th: SlackThreadSummary): string {
  const ws = th.workspace?.trim() || th.workspaceDomain?.trim() || t('slackThread.sidebar.other')
  const ch = th.channel?.trim().replace(/^#/, '') || th.channelId
  return ch ? `${ws} · #${ch}` : ws
}

const COLLAPSED_KEY = 'slackThreads.collapsed'

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

const tree = computed(() =>
  flat.value
    // Any other sort: one header-less workspace / channel.
    ? [{ key: 'flat', label: null, channels: [{ key: 'flat/ch', label: null, threads: flatThreads.value }] }]
    : groupThreads(props.threads),
)

function rowAge(th: SlackThreadSummary): string {
  return relativeAge(th.lastReplyAt ?? th.updatedAt)
}

function rowAgeTitle(th: SlackThreadSummary): string {
  return formatFullDate(th.lastReplyAt ?? th.updatedAt)
}
</script>

<template>
  <aside class="list-sidebar flex w-[360px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <!-- Search + pin / reload, filter + add -->
    <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
      <div class="flex items-center gap-1.5">
        <div class="relative min-w-0 flex-1">
          <Search
            class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <Input v-model="search" class="pl-8 pr-7" :placeholder="t('slackThread.sidebar.search')" />
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
        <!-- Reload the list + open thread from the DB (the MCP sidecar writes without events). -->
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
        <Button
          size="sm"
          class="w-[70px] shrink-0 px-0"
          :title="t('slackThread.sidebar.add')"
          :aria-label="t('slackThread.sidebar.add')"
          @click="emit('add')"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('common.new') }}
        </Button>
      </div>
      <div class="flex items-center gap-1.5">
        <SegmentedControl
          v-model="linkFilter"
          :options="linkFilterOptions"
          class="min-w-0 flex-1"
          :aria-label="t('slackThread.sidebar.linkFilter')"
        />
        <!-- Sort; tinted when not on the default. -->
        <DropdownMenu align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-xs"
              class="shrink-0"
              :class="sortMode !== DEFAULT_SORT && 'border-primary/50 text-primary'"
              :title="t('item.sortTitle')"
              :aria-label="t('item.sortTitle')"
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
        </DropdownMenu>
      </div>
    </div>

    <!-- Tree -->
    <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <Banner v-if="error" tone="destructive" class="mb-2">
        {{ error }}
        <template #actions>
          <Button
            variant="ghost"
            size="icon-xs"
            :title="t('slackThread.banner.retry')"
            :aria-label="t('slackThread.banner.retry')"
            @click="emit('retry')"
          >
            <RotateCw class="h-3 w-3" :stroke-width="2" />
          </Button>
        </template>
      </Banner>
      <div v-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">{{ t('slackThread.sidebar.empty') }}</p>
        <p class="mt-1 text-[11px] text-muted-foreground/70">{{ t('slackThread.sidebar.emptyHint') }}</p>
      </div>
      <p v-else-if="threads.length === 0" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        {{ t('slackThread.sidebar.noResults') }}
      </p>

      <div v-for="ws in tree" v-else :key="ws.key" class="mb-2">
        <button
          v-if="!flat"
          type="button"
          class="flex w-full cursor-pointer items-center gap-1 rounded px-1.5 py-1 text-left text-[13px] font-semibold text-foreground hover:bg-accent/50"
          :aria-expanded="!collapsed.has(ws.key)"
          :title="collapsed.has(ws.key) ? t('slackThread.sidebar.expand') : t('slackThread.sidebar.collapse')"
          @click="toggle(ws.key)"
        >
          <component
            :is="collapsed.has(ws.key) ? ChevronRight : ChevronDown"
            class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
            :stroke-width="2"
          />
          <span class="truncate">{{ ws.label ?? t('slackThread.sidebar.other') }}</span>
        </button>

        <template v-if="flat || !collapsed.has(ws.key)">
          <div v-for="ch in ws.channels" :key="ch.key" class="mt-0.5">
            <button
              v-if="!flat"
              type="button"
              class="flex w-full cursor-pointer items-center gap-1 rounded py-1 pl-4 pr-1.5 text-left text-xs text-muted-foreground hover:bg-accent/50 hover:text-foreground"
              :aria-expanded="!collapsed.has(ch.key)"
              @click="toggle(ch.key)"
            >
              <component
                :is="collapsed.has(ch.key) ? ChevronRight : ChevronDown"
                class="h-3 w-3 shrink-0"
                :stroke-width="2"
              />
              <Hash v-if="ch.label" class="h-3 w-3 shrink-0" :stroke-width="2" />
              <span class="truncate font-medium">
                {{ ch.label ? ch.label.slice(1) : t('slackThread.sidebar.other') }}
              </span>
            </button>

            <ul v-if="flat || !collapsed.has(ch.key)" :class="rowListClass">
              <li v-for="th in ch.threads" :key="th.id">
                <button
                  type="button"
                  class="group cursor-pointer flex-col pr-2 text-left"
                  :class="[flat ? 'pl-2' : 'pl-8', rowClass(th.id === selectedId)]"
                  :title="th.preview || th.title"
                  @click="emit('select', th.id)"
                >
                  <span class="flex w-full min-w-0 items-center gap-1.5">
                  <span
                    class="min-w-0 flex-1 truncate"
                    :class="[rowTitleClass, th.newCount > 0 && 'font-semibold text-foreground']"
                  >
                    {{ th.title || t('slackThread.untitled') }}
                  </span>
                  <span
                    v-if="th.legacy"
                    class="shrink-0 rounded border border-border px-1 text-[9px] font-semibold uppercase text-muted-foreground"
                  >
                    {{ t('slackThread.legacy.badge') }}
                  </span>
                  <SlackStatusIcon :thread="th" :progress="progress[th.id]" />
                  <span
                    v-if="th.linkedRunCount > 0"
                    class="flex shrink-0 items-center gap-0.5 text-[10px] tabular-nums text-muted-foreground"
                    :title="t('sessionLink.linkedCount', th.linkedRunCount)"
                  >
                    <Link2 class="h-3 w-3" :stroke-width="2" />
                    <template v-if="th.linkedRunCount > 1">{{ th.linkedRunCount }}</template>
                  </span>
                  <span
                    v-if="th.newCount > 0"
                    class="flex h-4 min-w-[16px] shrink-0 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-semibold leading-none text-primary-foreground"
                    :title="t('slackThread.sidebar.newCount', { count: th.newCount })"
                  >
                    {{ th.newCount }}
                  </span>
                  <span
                    class="shrink-0 text-[10px] tabular-nums text-muted-foreground/80"
                    :title="rowAgeTitle(th)"
                  >
                    {{ rowAge(th) }}
                  </span>
                  </span>
                  <!-- Flat list: where the thread lives. -->
                  <span v-if="flat" class="flex w-full min-w-0 items-center gap-1" :class="rowSubClass">
                    <Hash class="h-3 w-3 shrink-0" :stroke-width="2" />
                    <span class="truncate">{{ rowPlace(th) }}</span>
                  </span>
                </button>
              </li>
            </ul>
          </div>
        </template>
      </div>
    </div>
  </aside>
</template>
