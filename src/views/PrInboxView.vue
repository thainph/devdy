<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { emit as emitEvent } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import { usePrInboxStore, prKey, type PrInboxItem } from '@/stores/prInbox'
import { useRunsStore } from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useConductorStore } from '@/stores/conductor'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { resolvePermissionMode } from '@/lib/engineOptions'
import {
  Button, Skeleton, Input, ContextMenu, DropdownItem, DropdownSeparator,
} from '@/components/ui'
import PrInboxCard from '@/components/prInbox/PrInboxCard.vue'
import PrInboxFiltersPopover, { type PrFilterDimension } from '@/components/prInbox/PrInboxFiltersPopover.vue'
import PrInboxDisplayPopover from '@/components/prInbox/PrInboxDisplayPopover.vue'
import PrInboxBotAccountsModal from '@/components/prInbox/PrInboxBotAccountsModal.vue'
import {
  groupPrInbox,
  isGroupCollapsedByDefault,
  type PrGroup,
} from '@/lib/prInboxGrouping'
import {
  ambiguousRepoNames,
  applyFilters,
  buildFilterOptions,
  bulkReviewCandidates,
  canStartReview,
  effectiveAccountCount,
  effectiveFilters,
  makeAccountTone,
  hasActiveFilters,
  makeBotPredicate,
  normalizeLogin,
  reviewStatusOf,
  BULK_REVIEW_CAP,
  UNMAPPED_PROJECT_VALUE,
  type PrFilterOption,
} from '@/lib/prInboxFilters'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { useRunStatusEvents } from '@/composables/useRunStatusEvents'
import { WINDOW_OPEN_RUN, type WindowOpenRun } from '@/lib/sessionLinks'
import { PR_INBOX_WINDOW_OPEN_ROUTE, type PrInboxWindowOpenRoute } from '@/lib/prInboxWindow'
import {
  GitPullRequest, RefreshCw, Sparkles, User, AlertTriangle, Bot, ChevronRight, Folder, BookMarked,
  CircleDashed, EyeOff, KeyRound, ExternalLink, SearchX, Search, X, Pin, PinOff,
} from 'lucide-vue-next'

// Named explicitly (devtools / warnings): an inferred name is a build detail
// that minification can change.
defineOptions({ name: 'PrInboxView' })

const props = withDefaults(defineProps<{
  /** Rendered in its own OS window (PrInboxWindow): no router of the app and
   * no liveRuns stream there, so navigation becomes a request to the main
   * window (App.vue) and run status comes from useRunStatusEvents. */
  standalone?: boolean
}>(), { standalone: false })

const { t } = useI18n()
const router = useRouter()
const store = usePrInboxStore()
const runsStore = useRunsStore()
const live = useLiveRunsStore()
const conductorStore = useConductorStore()
const appSettings = useAppSettingsStore()
const githubAccounts = useGithubAccountsStore()
const { toast } = useToast()
const { confirm } = useConfirm()

// Shared clock for relative times + the stale badge, ticking once a minute so
// a long-open inbox doesn't freeze at "just now". The screen now lives in its
// own window (no <KeepAlive>), so plain mount / unmount bound it.
const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null

onMounted(() => {
  if (store.items.length === 0) store.refresh()
  // The configured account list decides whether cards show the "review as"
  // account chip. This window has its own Pinia, so Settings edits made in the
  // main window only arrive on a re-fetch (see the focus resync below).
  if (githubAccounts.accounts.length === 0) githubAccounts.fetch()
  clock = setInterval(() => { now.value = Date.now() }, 60_000)
})
onBeforeUnmount(() => {
  if (clock) clearInterval(clock)
  clock = null
})

// Standalone window: a light status feed (run:activated / run:done:<id>) for
// the PRs' linked review runs, instead of the main window's liveRuns.
const windowRuns = props.standalone
  ? useRunStatusEvents(() => store.items
    .filter(i => i.existing_run_id)
    .map(i => ({ id: i.existing_run_id!, status: i.existing_run_status })))
  : null

// Effective status for a PR's linked review run: a live streaming status (the
// liveRuns store, or the window's status feed) wins over the DB snapshot taken
// at refresh time.
function runStatus(item: PrInboxItem): string | null {
  if (!item.existing_run_id) return null
  const liveStatus = windowRuns
    ? windowRuns.status(item.existing_run_id)
    : live.get(item.existing_run_id)?.status
  return liveStatus ?? item.existing_run_status
}

// Standalone window: same window-priority behaviour as the other pop-outs
// (on top while focused, or pinned), and resync on focus — the inbox plus the
// GitHub account list (its own Pinia here). Throttled on the last ATTEMPT, not
// the last success: a refresh is a GitHub search per account, and a failing
// one must not re-fire on every focus.
const floating = props.standalone ? useFloatingWindow() : null
const FOCUS_REFRESH_MS = 60_000
let unlistenFocus: (() => void) | null = null
if (props.standalone) {
  onMounted(async () => {
    try {
      unlistenFocus = await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (!focused || store.loading || bulk.value !== null) return
        if (Date.now() - store.lastAttemptAt <= FOCUS_REFRESH_MS) return
        void store.refresh()
        void githubAccounts.fetch()
      })
    } catch {
      /* outside the Tauri shell */
    }
  })
  onBeforeUnmount(() => unlistenFocus?.())
}

// --- Filtering + grouping ---------------------------------------------------
// Pure logic lives in lib/prInboxFilters + lib/prInboxGrouping (unit-tested);
// this only wires it to the store and the live run status.

const isBot = computed(() => makeBotPredicate(store.botLogins))
const statusOf = (item: PrInboxItem) => reviewStatusOf(item, runStatus(item))
// Not persisted: a search is a one-off lookup, not a reading preference.
const search = ref('')

// Filter options come from what the user can see — hidden bots don't add
// options or inflate counts.
const filterOptions = computed(() => buildFilterOptions(
  store.hideBots ? store.items.filter(i => !isBot.value(i)) : store.items,
  statusOf,
))
const activeFilters = computed(() => effectiveFilters(store.filters, filterOptions.value))
const filtering = computed(() => hasActiveFilters(activeFilters.value) || search.value.trim() !== '')
const filtered = computed(() => applyFilters(store.items, activeFilters.value, search.value, statusOf))
const grouped = computed(() => groupPrInbox(filtered.value, {
  groupBy: store.groupBy,
  hideBots: store.hideBots,
  isBot: isBot.value,
}))

// Card context: bare `repo #num` unless the repo name is ambiguous across
// owners, and the "review as" account chip only when there is a choice.
const ambiguousRepos = computed(() => ambiguousRepoNames(store.items))
const showAccountChip = computed(() => effectiveAccountCount(githubAccounts.accounts, store.items) > 1)
const accountTone = computed(() => makeAccountTone(githubAccounts.accounts))

function setFilter(dimension: PrFilterDimension, values: string[]) {
  store.setFilters({ ...store.filters, [dimension]: values })
}

function clearAllFilters() {
  store.clearFilters()
  search.value = ''
}

function filterOptionLabel(dimension: PrFilterDimension, o: PrFilterOption): string {
  if (dimension === 'statuses') return t(`prInbox.status.${o.value}`)
  if (dimension === 'projects' && o.value === UNMAPPED_PROJECT_VALUE) return t('prInbox.notInDevdyGroup')
  return o.label
}

const FILTER_TITLE_KEYS: Record<PrFilterDimension, string> = {
  projects: 'prInbox.filters.project',
  authors: 'prInbox.filters.author',
  accounts: 'prInbox.filters.account',
  statuses: 'prInbox.filters.status',
}

// Removable chips for every filter value that currently applies.
const activeFilterChips = computed(() => (Object.keys(FILTER_TITLE_KEYS) as PrFilterDimension[]).flatMap(dimension =>
  (activeFilters.value[dimension] as string[]).map(value => {
    const option = filterOptions.value[dimension].find(o => o.value === value)
    return {
      key: `${dimension}:${value}`,
      dimension,
      value,
      title: t(FILTER_TITLE_KEYS[dimension]),
      label: option ? filterOptionLabel(dimension, option) : value,
    }
  }),
))

function removeFilterValue(dimension: PrFilterDimension, value: string) {
  setFilter(dimension, (store.filters[dimension] as string[]).filter(v => v !== value))
}

function isCollapsed(group: PrGroup): boolean {
  return store.collapsedGroups[group.key] ?? isGroupCollapsedByDefault(group)
}

function toggleGroup(group: PrGroup) {
  store.setGroupCollapsed(group.key, !isCollapsed(group))
}

function groupLabel(group: PrGroup): string {
  if (group.kind === 'unmapped') return t('prInbox.notInDevdyGroup')
  if (group.kind === 'bots') return t('prInbox.botsGroup')
  return group.label || t('prInbox.unknownGroup')
}

// --- Bots ---------------------------------------------------------------------

const botAccountsOpen = ref(false)

function isCustomBot(login: string): boolean {
  return store.botLogins.includes(normalizeLogin(login))
}

// Card menu (⋯ button / right-click). Teleported ContextMenu, since the card's
// overflow-hidden would clip an anchored dropdown.
const menu = ref<{ open: boolean; x: number; y: number; item: PrInboxItem | null }>({
  open: false, x: 0, y: 0, item: null,
})

function openMenu(item: PrInboxItem, x: number, y: number) {
  menu.value = { open: true, x, y, item }
}

function closeMenu() {
  menu.value = { ...menu.value, open: false }
}

function openPr(item: PrInboxItem) {
  openUrl(item.html_url).catch(() => { /* opener unavailable */ })
}

// --- Review actions -------------------------------------------------------

// Open the review session already linked to this PR.
// From the standalone window this asks the main window to show the run — its
// session pop-out if it has one, else the main window (WINDOW_OPEN_RUN, the
// Slack / Notes / Todos windows' mechanism, handled in App.vue).
function openRun(item: PrInboxItem) {
  if (!item.project_id || !item.existing_run_id) return
  if (props.standalone) {
    emitEvent(WINDOW_OPEN_RUN, {
      projectId: item.project_id,
      runId: item.existing_run_id,
    } satisfies WindowOpenRun).catch(() => {})
    return
  }
  router.push(`/projects/${item.project_id}/run/${item.existing_run_id}`)
}

// "Add to devdy": the project screen lives in the main window.
function openProjects() {
  if (props.standalone) {
    emitEvent(PR_INBOX_WINDOW_OPEN_ROUTE, { path: '/projects' } satisfies PrInboxWindowOpenRoute).catch(() => {})
    return
  }
  router.push('/projects')
}

// Create + launch a review run for a mapped PR with no linked run. Throws on
// failure; returns false when there was nothing to start (unmapped, already
// linked, or already being started).
async function startReview(item: PrInboxItem): Promise<boolean> {
  if (!item.mapped || !item.project_id || !item.repo_id || item.existing_run_id) return false
  const key = prKey(item)
  if (store.reviewingKeys.has(key)) return false
  store.setReviewing(key, true)
  try {
    // Allow reviewing a PR even when no linked issue can be resolved — the PR
    // inbox should run the review regardless of issue linkage.
    // Title the session with the PR's author and original title so inbox reviews
    // are told apart at a glance.
    const title = [`PR #${item.number}`, item.author_login && `@${item.author_login}`, item.title?.trim()]
      .filter(Boolean)
      .join(' · ')
    const run = await runsStore.fetchPr(
      item.project_id,
      item.repo_id,
      item.number,
      undefined,
      true,
      title,
      // Review as the account that was asked to review, not the project default.
      item.account_id,
    )
    // Every session is a conductor: launch it like RunView's Issue/PR Run button
    // — empty goal (default review prompt, labelled "PR #N"), default worker cap,
    // and the effective permission mode (this screen has no per-run pick).
    await conductorStore.start(run.id, '', {
      permissionModeOverride: resolvePermissionMode(null, appSettings.settings?.default_permission_mode),
    })
    // Link immediately so a second click resumes instead of duplicating. The run
    // streams in the background (liveRuns / ActiveRunsDock) — stay on this screen.
    store.linkRun(key, run.id, 'running')
    return true
  } finally {
    store.setReviewing(key, false)
  }
}

// Start a review, or resume the linked one. A PR that already has a review run
// (in DB or just created) never spawns a second session — we navigate to it.
async function handleReview(item: PrInboxItem) {
  if (item.existing_run_id) { openRun(item); return }
  try {
    if (await startReview(item)) {
      // Stay on this screen (no focus jump — the window is for multitasking):
      // the button turns into "Reviewing" / "View review", which opens it.
      toast.success(t('prInbox.reviewing', { repo: item.repo, number: item.number }))
    }
  } catch (e) {
    toast.error(String(e))
  }
}

// "AI review all" on a group header: confirm, then start the group's eligible
// PRs one at a time (sequential keeps the conductor + GitHub load sane), capped
// per click. One failure is reported and skipped, never stops the batch.
// Only PR keys are snapshotted: each item is re-resolved from the store right
// before it starts, so a refresh (which replaces store.items) can't feed a
// stale `existing_run_id = null` object back in and spawn a duplicate run.
// Refresh is also disabled while a batch runs.
const bulk = ref<{ groupKey: string; done: number; total: number } | null>(null)

function bulkCandidates(group: PrGroup) {
  return bulkReviewCandidates(group.items, item => store.reviewingKeys.has(prKey(item)))
}

async function reviewAll(group: PrGroup) {
  if (bulk.value) return
  const { items, total } = bulkCandidates(group)
  if (items.length === 0) return
  const keys = items.map(prKey)
  let message = t('prInbox.bulk.confirmBody', { count: items.length, group: groupLabel(group) }, items.length)
  if (total > items.length) message += `\n${t('prInbox.bulk.capped', { cap: BULK_REVIEW_CAP, total })}`
  const ok = await confirm({
    title: t('prInbox.bulk.confirmTitle'),
    message,
    confirmLabel: t('prInbox.bulk.confirmAction', items.length),
    cancelLabel: t('common.cancel'),
    variant: 'primary',
  })
  if (!ok) return
  const progress = { groupKey: group.key, done: 0, total: keys.length }
  bulk.value = { ...progress }
  let started = 0
  let failed = 0
  try {
    for (const key of keys) {
      const item = store.items.find(i => prKey(i) === key)
      // Gone, already linked to a run, or being started elsewhere → skip.
      if (canStartReview(item, store.reviewingKeys.has(key))) {
        try {
          if (await startReview(item)) started += 1
        } catch (e) {
          failed += 1
          toast.error(t('prInbox.bulk.itemFailed', { repo: item.repo, number: item.number, error: String(e) }))
        }
      }
      progress.done += 1
      bulk.value = { ...progress }
    }
  } finally {
    bulk.value = null
  }
  if (started > 0) toast.success(t('prInbox.bulk.started', { count: started }, started))
  if (failed > 0 && started === 0) toast.error(t('prInbox.bulk.allFailed', { count: failed }, failed))
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">{{ t('prInbox.title') }}</h1>
        <span
          v-if="!store.loading && store.items.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
          :title="grouped.visibleCount < store.items.length ? t('prInbox.visibleOfTotal', { visible: grouped.visibleCount, total: store.items.length }) : undefined"
        >
          {{ grouped.visibleCount < store.items.length ? `${grouped.visibleCount} / ${store.items.length}` : store.items.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <!-- Standalone window: pin on top (same control as the Slack window) -->
        <Button
          v-if="floating"
          :variant="floating.pinned.value ? 'primary' : 'outline'"
          size="icon-sm"
          :aria-pressed="floating.pinned.value"
          :title="floating.pinned.value ? t('item.unpin') : t('item.pin')"
          :aria-label="floating.pinned.value ? t('item.unpin') : t('item.pin')"
          @click="floating.togglePin()"
        >
          <component :is="floating.pinned.value ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button variant="ghost" :disabled="store.loading || bulk !== null" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="2" />
          {{ t('common.refresh') }}
        </Button>
      </div>
    </div>

    <!-- Toolbar: search + Filters / Display popovers (patterned on
         CalendarView's display-options popover). A size container so the
         buttons drop to icon-only when the content pane — not the window — is
         narrow (the sidebar eats into it). `relative z-10`: the size container
         is its own stacking context, so lift it above the (positioned) cards
         its popovers overlap. Everything but the search is persisted.
         The @[30rem]/toolbar icon-only fallback rarely triggers: the window's
         minWidth is 900px, leaving the content pane at roughly ≥ 680px; it
         covers a wide sidebar or a future smaller minimum. -->
    <div v-if="store.items.length > 0" class="@container/toolbar relative z-10 px-6 py-2 border-b border-border/60 shrink-0">
      <div class="flex min-w-0 items-center gap-2">
        <!-- Search: same icon + clear pattern as ItemSidebar / SlackSidebar -->
        <div class="relative min-w-32 max-w-md flex-1">
          <Search
            class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <Input
            v-model="search"
            class="pl-8 pr-7"
            :placeholder="t('prInbox.searchPlaceholder')"
            :aria-label="t('prInbox.searchPlaceholder')"
            @keydown.esc="search = ''"
          />
          <button
            v-if="search"
            type="button"
            class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
            :title="t('prInbox.clearSearch')"
            :aria-label="t('prInbox.clearSearch')"
            @click="search = ''"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>
        <div class="ml-auto flex shrink-0 items-center gap-2">
          <PrInboxFiltersPopover
            :options="filterOptions"
            :model-value="store.filters"
            :active-count="activeFilterChips.length"
            :option-label="filterOptionLabel"
            @update="setFilter"
            @clear="store.clearFilters()"
          />
          <PrInboxDisplayPopover @manage-bots="botAccountsOpen = true" />
        </div>
      </div>
    </div>

    <!-- Active filters: one removable chip per applied value (chip look of
         SkillsView's group filter chips). -->
    <div
      v-if="activeFilterChips.length > 0"
      class="flex flex-wrap items-center gap-1.5 px-6 py-2 border-b border-border/60 shrink-0"
    >
      <span
        v-for="chip in activeFilterChips"
        :key="chip.key"
        class="inline-flex max-w-full items-center gap-1 rounded-md bg-muted py-1 pl-2 pr-1 text-[11px] text-foreground"
      >
        <span class="shrink-0 text-muted-foreground">{{ chip.title }}:</span>
        <span class="min-w-0 truncate max-w-40" :title="chip.label">{{ chip.label }}</span>
        <button
          type="button"
          class="shrink-0 rounded p-0.5 cursor-pointer text-muted-foreground hover:bg-accent hover:text-foreground transition-colors"
          :title="t('prInbox.filters.remove', { filter: `${chip.title}: ${chip.label}` })"
          :aria-label="t('prInbox.filters.remove', { filter: `${chip.title}: ${chip.label}` })"
          @click="removeFilterValue(chip.dimension, chip.value)"
        >
          <X class="h-3 w-3" :stroke-width="2" />
        </button>
      </span>
      <button
        type="button"
        class="ml-1 text-[11px] font-medium text-primary hover:underline cursor-pointer"
        @click="store.clearFilters()"
      >
        {{ t('prInbox.filters.clearAll') }}
      </button>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Partial-failure warning: some accounts failed to load (rate limit,
           network, bad PAT). Shown above the list so a missing PR reads as
           "some accounts failed, try refresh" rather than "no PRs". -->
      <div
        v-if="store.warnings.length > 0"
        class="mb-4 flex items-start gap-2.5 rounded-lg border border-amber-500/25 bg-amber-500/10 p-3 text-xs text-amber-600 dark:text-amber-400"
      >
        <AlertTriangle class="h-4 w-4 shrink-0 mt-px" :stroke-width="2" />
        <div class="min-w-0 flex-1">
          <p class="font-medium">{{ t('prInbox.partialFailTitle', store.warnings.length) }}</p>
          <p class="mt-0.5 text-amber-600/80 dark:text-amber-400/80">
            {{ store.warnings.map(w => w.account_label).join(', ') }} — {{ t('prInbox.partialFailBody') }}
          </p>
        </div>
        <Button variant="ghost" size="xs" :disabled="store.loading || bulk !== null" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="2" />
          {{ t('common.refresh') }}
        </Button>
      </div>

      <!-- Loading skeleton — mirrors the PR card layout so the wait reads as
           "content incoming". Rows cascade in with a small stagger. -->
      <div v-if="store.loading && store.items.length === 0" class="flex flex-col gap-3">
        <div
          v-for="(w, i) in [72, 54, 80, 60, 66]" :key="i"
          class="animate-fade-rise flex items-center gap-4 rounded-lg border border-border/60 bg-card p-4"
          :style="{ animationDelay: `${i * 70}ms` }"
        >
          <Skeleton class="h-9 w-9 shrink-0 rounded-lg" />
          <div class="min-w-0 flex-1 space-y-2.5">
            <Skeleton class="h-3.5 rounded" :style="{ width: `${w}%` }" />
            <div class="flex items-center gap-2">
              <Skeleton class="h-2.5 w-28 rounded" />
              <Skeleton class="h-2.5 w-16 rounded" />
              <Skeleton class="h-2.5 w-20 rounded" />
            </div>
          </div>
          <Skeleton class="h-7 w-24 shrink-0 rounded-md" />
        </div>
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="store.items.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <GitPullRequest class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('prInbox.emptyTitle') }}</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-64">
          {{ t('prInbox.emptyBody') }}
        </p>
        <Button class="mt-4" variant="ghost" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('common.refresh') }}
        </Button>
      </div>

      <!-- PR list, grouped per the toolbar. `none` mode is one headerless
           group. When every PR is a hidden bot PR there are no groups, only
           the summary row below — not the "no PRs" empty state. -->
      <!-- `@container/list`: group headers and PrInboxCard size themselves
           against the list width (36rem / 44rem / 52rem steps). -->
      <div v-else class="@container/list flex flex-col gap-5">
        <!-- Filters / search matched nothing (hidden bots aside). Clears the
             search too, hence its own label (the chip row's "Clear filters"
             leaves the search alone). -->
        <div
          v-if="grouped.groups.length === 0 && filtering"
          class="flex items-center gap-2 rounded-lg border border-dashed border-border/60 px-3 py-3 text-xs text-muted-foreground"
        >
          <SearchX class="h-4 w-4 shrink-0" :stroke-width="1.75" />
          <span>{{ t('prInbox.filters.noMatch') }}</span>
          <span class="text-muted-foreground/50">·</span>
          <button type="button" class="font-medium text-primary hover:underline cursor-pointer" @click="clearAllFilters">
            {{ t('prInbox.filters.clearSearchAndFilters') }}
          </button>
        </div>

        <section v-for="group in grouped.groups" :key="group.key" class="flex flex-col gap-2">
          <!-- Header: the name truncates first (counts stay); "N unreviewed"
               drops below 36rem, "AI review all" goes icon-only below 44rem,
               "Mark as bot" takes no space until hover / keyboard focus. -->
          <div v-if="group.kind !== 'all'" class="group/header flex items-center gap-2 min-w-0">
            <button
              type="button"
              class="flex min-w-0 flex-1 items-center gap-2 text-left cursor-pointer"
              :aria-expanded="!isCollapsed(group)"
              @click="toggleGroup(group)"
            >
              <ChevronRight
                class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
                :class="{ 'rotate-90': !isCollapsed(group) }"
                :stroke-width="2"
              />
              <img
                v-if="group.avatarUrl"
                :src="group.avatarUrl"
                alt=""
                class="h-4 w-4 shrink-0 rounded-full bg-muted"
              />
              <Bot v-else-if="group.kind === 'bots'" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <CircleDashed v-else-if="group.kind === 'unmapped'" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <Folder v-else-if="store.groupBy === 'project'" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <BookMarked v-else-if="store.groupBy === 'repo'" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <KeyRound v-else-if="store.groupBy === 'reviewer_account'" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <User v-else class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <span
                class="min-w-0 text-xs font-semibold truncate group-hover/header:text-primary transition-colors"
                :class="{ 'font-mono': store.groupBy === 'repo' && group.kind === 'normal' }"
              >
                {{ groupLabel(group) }}
              </span>
              <span class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground">
                {{ group.items.length }}
              </span>
              <span v-if="group.unreviewedCount > 0" class="hidden @[36rem]/list:inline shrink-0 text-[10px] text-amber-600 dark:text-amber-400">
                {{ t('prInbox.unreviewed', group.unreviewedCount) }}
              </span>
            </button>

            <div class="flex items-center gap-1 shrink-0">
              <!-- Author group: treat this login as a bot from now on. Space is
                   reserved (visibility, not display) so the group name doesn't
                   shift on hover; revealed on hover or when the header holds
                   keyboard focus. The card ⋯ menu offers the same action. -->
              <Button
                v-if="store.groupBy === 'author' && group.kind === 'normal' && group.label"
                variant="ghost"
                size="icon-sm"
                class="invisible group-hover/header:visible group-focus-within/header:visible"
                :title="t('prInbox.markAsBot')"
                :aria-label="t('prInbox.markAsBot')"
                @click="store.addBotLogin(group.label)"
              >
                <Bot class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <!-- Bulk review: eligible = mapped, no linked run, not starting -->
              <Button
                v-if="bulk?.groupKey === group.key"
                variant="outline"
                size="xs"
                disabled
                :title="t('prInbox.bulk.progress', { done: bulk.done, total: bulk.total })"
              >
                <RefreshCw class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
                <span class="hidden @[44rem]/list:inline">{{ t('prInbox.bulk.progress', { done: bulk.done, total: bulk.total }) }}</span>
                <span class="tabular-nums @[44rem]/list:hidden">{{ bulk.done }}/{{ bulk.total }}</span>
              </Button>
              <Button
                v-else-if="bulkCandidates(group).total > 0"
                variant="outline"
                size="xs"
                :disabled="bulk !== null"
                :title="t('prInbox.bulk.action', { count: Math.min(bulkCandidates(group).total, BULK_REVIEW_CAP) })"
                :aria-label="t('prInbox.bulk.action', { count: Math.min(bulkCandidates(group).total, BULK_REVIEW_CAP) })"
                @click="reviewAll(group)"
              >
                <Sparkles class="h-3.5 w-3.5" :stroke-width="2" />
                <span class="hidden @[44rem]/list:inline">
                  {{ t('prInbox.bulk.action', { count: Math.min(bulkCandidates(group).total, BULK_REVIEW_CAP) }) }}
                </span>
              </Button>
            </div>
          </div>

          <div
            v-if="group.kind === 'all' || !isCollapsed(group)"
            class="flex flex-col"
            :class="store.density === 'compact' ? 'gap-1.5' : group.kind === 'all' ? 'gap-3' : 'gap-2'"
          >
            <!-- Keyed per group: in reviewer-account mode one PR can sit in
                 two groups, but never twice in the same one. -->
            <PrInboxCard
              v-for="(item, i) in group.items"
              :key="`${group.key}:${prKey(item)}`"
              :item="item"
              :density="store.density"
              :group-by="group.kind === 'normal' ? store.groupBy : 'none'"
              :show-owner="ambiguousRepos.has(item.repo.toLowerCase())"
              :show-account="showAccountChip"
              :account-tone="accountTone"
              :grouped="group.kind !== 'all'"
              :is-bot="isBot(item)"
              :run-status="runStatus(item)"
              :reviewing="store.reviewingKeys.has(prKey(item))"
              :now="now"
              :index="i"
              @review="handleReview"
              @open-run="openRun"
              @add-to-devdy="openProjects"
              @menu="openMenu"
            />
          </div>
        </section>

        <!-- Hidden bot PRs: one summary row instead of the noise. -->
        <div
          v-if="grouped.hiddenBotCount > 0"
          class="flex items-center gap-2 rounded-lg border border-dashed border-border/60 px-3 py-2 text-[11px] text-muted-foreground"
        >
          <EyeOff class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span>{{ t('prInbox.botsHidden', grouped.hiddenBotCount) }}</span>
          <span class="text-muted-foreground/50">·</span>
          <button
            type="button"
            class="font-medium text-primary hover:underline cursor-pointer"
            @click="store.setHideBots(false)"
          >
            {{ t('prInbox.showBots') }}
          </button>
        </div>
      </div>
    </div>

    <!-- Card menu (⋯ / right-click) -->
    <ContextMenu :open="menu.open" :x="menu.x" :y="menu.y" @close="closeMenu">
      <template v-if="menu.item">
        <DropdownItem @click="openPr(menu.item)">
          <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('prInbox.openOnGitHub') }}
        </DropdownItem>
        <DropdownSeparator />
        <DropdownItem v-if="isCustomBot(menu.item.author_login)" @click="store.removeBotLogin(menu.item.author_login)">
          <User class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('prInbox.unmarkBot', { login: menu.item.author_login }) }}
        </DropdownItem>
        <DropdownItem v-else-if="!menu.item.author_is_bot" @click="store.addBotLogin(menu.item.author_login)">
          <Bot class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('prInbox.markLoginAsBot', { login: menu.item.author_login }) }}
        </DropdownItem>
        <DropdownItem v-else disabled>
          <Bot class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('prInbox.detectedBot') }}
        </DropdownItem>
      </template>
    </ContextMenu>

    <PrInboxBotAccountsModal :open="botAccountsOpen" @close="botAccountsOpen = false" />
  </div>
</template>
