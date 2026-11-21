<script setup lang="ts">
// Left pane of the Slack threads window, built from the shared list-sidebar
// pieces (components/listDetail):
//
//   Slack threads 12 [3 unread]               [+ New] [⋯]
//   ( 🔍 Search threads…  (⌘F)                     ⚙︎• )
//   Filtering: Unread · icontruction         Clear filters   ← only while filtering
//   ─────────────────────────────────────────────────────
//   ▾ ACME                                                 5   ← "By channel" only
//     ▾ # general                                          3
//         thread row …
//
// The "N unread" badge toggles the unread filter (the one reached for most).
// Everything else about the view — activity (with counts), project, sort and
// "Reset to defaults" — lives in the one menu behind the search box's sliders
// button (lib/slackListView; the window owns the view and remembers its sort).
// ⋯ holds Reload, the pop-out's pin / dock, and Hide the list.
//
// Sorted "By channel", threads sit in a workspace → #channel tree (each header
// collapsible, remembered in localStorage); any other sort gives one flat list,
// each row naming its workspace / channel. A row shows the title, the last
// reply's age (full date in the tooltip), a new-message badge, a status mark
// (spinner / error / warning), a link mark with the linked-session count and an
// "Old" badge for legacy rows.
import { computed, ref } from 'vue'
import { useListPaneSidebar } from '@/composables/useListPane'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ChevronRight, Hash, Link2, RotateCw } from 'lucide-vue-next'
import { Banner, Button, DropdownItem, DropdownSeparator } from '@/components/ui'
import ListSidebarHeader from '@/components/listDetail/ListSidebarHeader.vue'
import ListSearchBox from '@/components/listDetail/ListSearchBox.vue'
import ListFilterSummary from '@/components/listDetail/ListFilterSummary.vue'
import ListGroupHeader from '@/components/listDetail/ListGroupHeader.vue'
import ListMenuLabel from '@/components/listDetail/ListMenuLabel.vue'
import ListMenuCheck from '@/components/listDetail/ListMenuCheck.vue'
import PopoutMenuItems from '@/components/listDetail/PopoutMenuItems.vue'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate, groupThreads, type ActivityFilter } from '@/lib/slackThreadUtils'
import {
  SLACK_ACTIVITIES,
  SLACK_NO_PROJECT,
  SLACK_SORTS,
  clearSlackFilters,
  defaultSlackListView,
  slackFilterSummary,
  slackViewDiffersFromDefault,
  sortSlackThreads,
  type SlackListView,
  type SlackSortMode,
} from '@/lib/slackListView'
import type { SlackThreadProgress, SlackThreadSummary } from '@/stores/slackThreads'
import { rowClass, rowListClass, rowSubClass, rowTitleClass } from '@/components/listDetail/rowStyles'
import SlackStatusIcon from './SlackStatusIcon.vue'

const props = defineProps<{
  /** Already filtered by the view and the search. */
  threads: SlackThreadSummary[]
  /** True when there are no threads at all (vs. none matching the filters). */
  emptyStore: boolean
  selectedId: string | null
  progress: Record<string, SlackThreadProgress>
  /** The projects the filter menu offers (id + name), "All" / "No project" excluded. */
  projectOptions: { value: string; label: string }[]
  /** The list failed to load (shown above the rows with a retry). */
  error: string | null
  /** Window pinned above other apps (useFloatingWindow). */
  pinned: boolean
  /** A full reload is in flight. */
  reloading: boolean
  /** Rendered inside the main window: no window pin / dock items. */
  embedded?: boolean
  /** Counts behind the filter menu's activities (lib/slackListView slackActivityCounts). */
  counts: Record<ActivityFilter, number>
}>()

const search = defineModel<string>('search', { required: true })
const view = defineModel<SlackListView>('view', { required: true })

const emit = defineEmits<{
  select: [id: string]
  add: []
  retry: []
  togglePin: []
  /** Pop-out: move into the main window as a tab (lib/surfaceDock). */
  dock: []
  reload: []
}>()

const { t } = useI18n()

// Hideable per screen (⌘B) when the host provides a list pane.
const rootEl = ref<HTMLElement | null>(null)
const listPane = useListPaneSidebar(rootEl)

// ── View (the filter menu) ──────────────────────────────────────────────────

function patch(p: Partial<SlackListView>) {
  view.value = { ...view.value, ...p }
}

const ACTIVITY_LABEL: Record<ActivityFilter, string> = {
  '': 'slackThread.sidebar.filterAll',
  unread: 'slackThread.sidebar.filterUnread',
  recent: 'slackThread.sidebar.filterRecent',
}
const ACTIVITY_DOT: Partial<Record<ActivityFilter, string>> = { unread: 'bg-primary' }
const SORT_LABEL: Record<SlackSortMode, string> = {
  recent: 'slackThread.sidebar.sortByRecent',
  channel: 'slackThread.sidebar.sortByChannel',
  title: 'slackThread.sidebar.sortByTitle',
}

const projectChoices = computed(() => [
  { value: '', label: t('slackThread.sidebar.allProjects') },
  ...props.projectOptions,
  { value: SLACK_NO_PROJECT, label: t('slackThread.sidebar.noProject') },
])
function projectLabel(id: string): string {
  return projectChoices.value.find((o) => o.value === id)?.label ?? id
}

const changed = computed(() => slackViewDiffersFromDefault(view.value))
const summary = computed(() =>
  slackFilterSummary(view.value, { activity: (a) => t(ACTIVITY_LABEL[a]), project: projectLabel }),
)
const unread = computed(() => props.counts.unread)

function toggleUnread() {
  patch({ activity: view.value.activity === 'unread' ? '' : 'unread' })
}
function clearFilters() {
  search.value = ''
  view.value = clearSlackFilters(view.value)
}
function resetView() {
  view.value = defaultSlackListView()
}

// ── The workspace → channel tree, or one flat list by another sort ─────────

const flat = computed(() => view.value.sort !== 'channel')

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
    ? [{
        key: 'flat',
        label: null,
        count: props.threads.length,
        channels: [{ key: 'flat/ch', label: null, threads: sortSlackThreads(props.threads, view.value.sort) }],
      }]
    : groupThreads(props.threads).map((ws) => ({
        ...ws,
        count: ws.channels.reduce((n, ch) => n + ch.threads.length, 0),
      })),
)

function rowAge(th: SlackThreadSummary): string {
  return relativeAge(th.lastReplyAt ?? th.updatedAt)
}

function rowAgeTitle(th: SlackThreadSummary): string {
  return formatFullDate(th.lastReplyAt ?? th.updatedAt)
}
</script>

<template>
  <aside
    v-if="listPane.shown.value"
    ref="rootEl"
    class="list-sidebar flex shrink-0 flex-col border-r border-border/60 bg-sidebar"
    :class="[listPane.widthClass.value, listPane.floatClass.value]"
  >
    <ListSidebarHeader
      :title="t('slackThread.nav')"
      :count="threads.length"
      :new-label="t('common.new')"
      :new-title="t('slackThread.sidebar.add')"
      @new="emit('add')"
    >
      <template v-if="unread > 0 || view.activity === 'unread'" #badge>
        <button
          type="button"
          class="h-5 shrink-0 cursor-pointer rounded-full px-2 text-[11px] font-semibold tabular-nums transition-colors"
          :class="view.activity === 'unread'
            ? 'bg-primary text-primary-foreground'
            : 'bg-primary/12 text-primary hover:bg-primary/20'"
          :aria-pressed="view.activity === 'unread'"
          :title="t('slackThread.sidebar.unreadBadgeTitle')"
          @click="toggleUnread"
        >
          {{ t('slackThread.sidebar.unreadCount', { n: unread }) }}
        </button>
      </template>
      <template #menu>
        <!-- Reload the list + open thread from the DB (the MCP sidecar writes without events). -->
        <DropdownItem :disabled="reloading" @click="emit('reload')">
          <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': reloading }" :stroke-width="1.75" />
          {{ t('item.reload') }}
        </DropdownItem>
        <PopoutMenuItems v-if="!embedded" :pinned="pinned" @toggle-pin="emit('togglePin')" @dock="emit('dock')" />
      </template>
    </ListSidebarHeader>

    <div class="shrink-0 px-2.5 pb-1.5 pt-0.5">
      <ListSearchBox v-model="search" :placeholder="t('slackThread.sidebar.search')" :filter-active="changed">
        <template #filters>
          <ListMenuLabel>{{ t('slackThread.sidebar.activityTitle') }}</ListMenuLabel>
          <ListMenuCheck
            v-for="a in SLACK_ACTIVITIES"
            :key="a || 'all'"
            radio
            :checked="view.activity === a"
            :dot="ACTIVITY_DOT[a]"
            :count="counts[a]"
            @click="patch({ activity: a })"
          >
            {{ t(ACTIVITY_LABEL[a]) }}
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
            v-for="s in SLACK_SORTS"
            :key="s"
            radio
            :checked="view.sort === s"
            @click="patch({ sort: s })"
          >
            {{ t(SORT_LABEL[s]) }}
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
      <div v-else-if="threads.length === 0" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        <p>{{ t('slackThread.sidebar.noResults') }}</p>
        <button
          v-if="search || summary.length"
          type="button"
          class="mt-1 cursor-pointer underline underline-offset-2 hover:text-foreground"
          @click="clearFilters"
        >
          {{ t('listPane.clearFilters') }}
        </button>
      </div>

      <template v-for="ws in tree" v-else :key="ws.key">
        <ListGroupHeader
          v-if="!flat"
          :label="ws.label ?? t('slackThread.sidebar.other')"
          :count="ws.count"
          :collapsed="collapsed.has(ws.key)"
          @toggle="toggle(ws.key)"
        />

        <div v-if="flat || !collapsed.has(ws.key)" :class="flat ? 'mt-1.5' : 'mb-2'">
          <div v-for="ch in ws.channels" :key="ch.key">
            <button
              v-if="!flat"
              type="button"
              class="flex w-full cursor-pointer items-center gap-1 rounded py-1 pl-3 pr-2 text-left text-xs text-muted-foreground transition-colors hover:text-foreground"
              :aria-expanded="!collapsed.has(ch.key)"
              @click="toggle(ch.key)"
            >
              <component
                :is="collapsed.has(ch.key) ? ChevronRight : ChevronDown"
                class="h-3 w-3 shrink-0"
                :stroke-width="2"
              />
              <Hash v-if="ch.label" class="h-3 w-3 shrink-0" :stroke-width="2" />
              <span class="min-w-0 flex-1 truncate font-medium">
                {{ ch.label ? ch.label.slice(1) : t('slackThread.sidebar.other') }}
              </span>
              <span class="shrink-0 text-[10px] tabular-nums">{{ ch.threads.length }}</span>
            </button>

            <ul v-if="flat || !collapsed.has(ch.key)" :class="rowListClass">
              <li v-for="th in ch.threads" :key="th.id">
                <button
                  type="button"
                  class="group cursor-pointer flex-col pr-2 text-left"
                  :class="[flat ? 'pl-2' : 'pl-7', rowClass(th.id === selectedId)]"
                  :title="th.preview || th.title"
                  @click="emit('select', th.id); listPane.picked()"
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
        </div>
      </template>
    </div>
  </aside>
</template>
