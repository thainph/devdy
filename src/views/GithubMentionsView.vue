<script setup lang="ts">
// GitHub Mentions: comments on GitHub that @-mention the user directly and
// wait for their answer, across all linked GitHub accounts. Header layout follows the PR Inbox:
// search + one "Filter & display" button beside Refresh. Mentions are split into tabs by
// what they need: "Needs reply" (the badge), "FYI" and "Done" (answered in the
// last week, closed / merged, acknowledged) — sorted by the backend's rules
// and AI, movable by hand from the card menu. "Check with AI" opens a
// session on the issue / PR (like the PR Inbox's AI review), starting with the
// configurable check prompt; the user answers on GitHub themselves.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import DockWindowButton from '@/components/surfaces/DockWindowButton.vue'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import { emit as emitEvent } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Banner, Button, ContextMenu, DropdownItem, DropdownSeparator, PageHeader, PageHeaderSearch, SegmentedControl, Skeleton } from '@/components/ui'
import MentionCard from '@/components/githubMentions/MentionCard.vue'
import InboxGroupHeader from '@/components/inbox/InboxGroupHeader.vue'
import InboxList from '@/components/inbox/InboxList.vue'
import { useInboxKeyboard } from '@/composables/useInboxKeyboard'
import MentionOptionsMenu from '@/components/githubMentions/MentionOptionsMenu.vue'
import ListFilterSummary from '@/components/listDetail/ListFilterSummary.vue'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import PromptConfigModal from '@/components/PromptConfigModal.vue'
import TodoCaptureModal from '@/components/focus/TodoCaptureModal.vue'
import type { TodoPrefill } from '@/lib/focus'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { useToast } from '@/composables/useToast'
import { useRunsStore } from '@/stores/runs'
import { useConductorStore } from '@/stores/conductor'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { ambiguousRepoNames, makeAccountTone } from '@/lib/prInboxFilters'
import { resolvePermissionMode } from '@/lib/engineOptions'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { useRunStatusEvents } from '@/composables/useRunStatusEvents'
import { WINDOW_OPEN_RUN, type WindowOpenRun } from '@/lib/sessionLinks'
import {
  activeFilters,
  applyFilters,
  categoryOf,
  checkGoal,
  checkStatusOf,
  filterOptions,
  groupMentions,
  MENTION_CATEGORIES,
  MENTION_REASONS,
  splitByCategory,
  type MentionCategory,
  type MentionFilterDimension,
  type MentionFilterOption,
  type MentionGroup,
  type MentionItem,
} from '@/lib/githubMentions'
import {
  AtSign, BellOff, BookMarked, Check, CheckCheck, CircleDashed, Copy, Folder, EyeOff, ExternalLink, FolderX, Info, ListPlus, Loader2, MailOpen, RefreshCw, Reply,
  SearchX, Undo2,
} from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  /** Rendered in its own OS window (views/GithubMentionsWindow) rather than the main one. */
  standalone?: boolean
}>(), { standalone: false })

const { t } = useI18n()
const { toast } = useToast()
const store = useGithubMentionsStore()
const router = useRouter()
const runsStore = useRunsStore()
const conductorStore = useConductorStore()
const appSettings = useAppSettingsStore()
const live = useLiveRunsStore()
const githubAccounts = useGithubAccountsStore()

const search = ref('')
const collapsed = ref<Set<string>>(new Set())

onMounted(() => {
  void store.load()
  void store.loadMutes()
  if (githubAccounts.accounts.length === 0) void githubAccounts.fetch()
})

// Standalone window: on top while focused (like the other pop-outs), a light
// status feed for the linked AI-check sessions instead of the main window's
// liveRuns, and a refresh on focus when the list is over a minute old.
if (props.standalone) useFloatingWindow()
const windowRuns = props.standalone
  ? useRunStatusEvents(() => store.items
    .filter(i => i.existing_run_id)
    .map(i => ({ id: i.existing_run_id!, status: i.existing_run_status })))
  : null
const FOCUS_REFRESH_MS = 60_000
let unlistenFocus: (() => void) | null = null
if (props.standalone) {
  onMounted(async () => {
    try {
      unlistenFocus = await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (!focused || store.loading) return
        const at = store.fetchedAt ? Date.parse(store.fetchedAt) : 0
        if (Date.now() - at > FOCUS_REFRESH_MS) void store.refresh()
      })
    } catch {
      /* outside the Tauri shell */
    }
  })
  onBeforeUnmount(() => unlistenFocus?.())
}

const options = computed(() => filterOptions(store.items))
const effective = computed(() => activeFilters(store.filters, store.items))
const activeCount = computed(() => Object.values(effective.value).reduce((n, v) => n + v.length, 0))
const visible = computed(() => applyFilters(store.items, effective.value, search.value))
const split = computed(() => splitByCategory(visible.value))
const groups = computed(() => groupMentions(split.value[store.tab], store.groupBy))

const CATEGORY_ICONS = { reply: Reply, fyi: Info, done: CheckCheck } as const
const tabOptions = computed(() => MENTION_CATEGORIES.map(c => ({
  value: c,
  label: `${t(`githubMentions.tab.${c}`)} · ${split.value[c].length}`,
  icon: CATEGORY_ICONS[c],
  title: t(`githubMentions.tab.${c}Hint`),
})))

function optionLabel(d: MentionFilterDimension, o: MentionFilterOption): string {
  if (d === 'reasons' && (MENTION_REASONS as string[]).includes(o.value)) return t(`githubMentions.reason.${o.value}`)
  return o.label
}

/** "Reason: Tagged you · Repository: org/api" for the line under the header. */
const filterParts = computed(() =>
  (Object.keys(effective.value) as MentionFilterDimension[]).flatMap(d =>
    effective.value[d].map(value => {
      const o = options.value[d].find(x => x.value === value)
      const title = t(`githubMentions.filters.${d === 'reasons' ? 'reason' : d === 'accounts' ? 'account' : 'repo'}`)
      return `${title}: ${o ? optionLabel(d, o) : value}`
    }),
  ),
)

function groupLabel(g: MentionGroup): string {
  if (g.unmapped) return t('githubMentions.notInDevdyGroup')
  if (g.label) return g.label
  return store.groupBy === 'person' ? t('githubMentions.unknownAuthor') : ''
}

function toggleGroup(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
}

// Card display, as in the PR Inbox: owner only for ambiguous repo names, the
// tagged account (stable colour dot) only when several accounts are in play.
const ambiguousRepos = computed(() => ambiguousRepoNames(store.items))
const accountTone = computed(() => makeAccountTone(githubAccounts.accounts))
const showAccount = computed(() => new Set(store.items.map(i => i.account_id)).size > 1)

// ── Check with AI ────────────────────────────────────────────────────────────
// Live status (the liveRuns store) wins over the snapshot taken at fetch time.
function runStatus(item: MentionItem): string | null {
  const id = item.existing_run_id
  if (!id) return null
  const liveStatus = windowRuns ? windowRuns.status(id) : live.get(id)?.status
  return liveStatus ?? item.existing_run_status
}
function checkStatus(item: MentionItem) {
  return checkStatusOf(item, runStatus(item))
}

// From the standalone window this asks the main window to show the run
// (WINDOW_OPEN_RUN, handled in App.vue), like the PR Inbox window.
function openRun(item: MentionItem) {
  if (!item.project_id || !item.existing_run_id) return
  if (props.standalone) {
    emitEvent(WINDOW_OPEN_RUN, { projectId: item.project_id, runId: item.existing_run_id } satisfies WindowOpenRun)
      .catch(() => {})
    return
  }
  live.markSeen(item.existing_run_id)
  router.push(`/projects/${item.project_id}/run/${item.existing_run_id}`).catch(() => {})
}

// The "Check with AI" prompt editor (Display → Check prompt), like the PR Inbox's.
const promptEditorOpen = ref(false)

// Same path as the PR Inbox's AI review: fetch the issue / PR into a run (as
// the account that was tagged), start it as a conductor with our goal, then
// link it so the next click opens it. Stays on this screen.
async function check(item: MentionItem) {
  const status = checkStatus(item)
  if (status === 'running' || status === 'done') { openRun(item); return }
  if (status !== 'none' || item.number === null || !item.project_id || !item.repo_id) return
  if (store.busyKeys.has(item.key)) return
  const { project_id: projectId, repo_id: repoId, number } = item
  try {
    await store.withBusy(item.key, async () => {
      const title = [t('githubMentions.check.sessionTitle'), `#${number}`, item.trigger && `@${item.trigger.author_login}`, item.title.trim()]
        .filter(Boolean)
        .join(' · ')
      const run = item.kind === 'PullRequest'
        ? await runsStore.fetchPr(projectId, repoId, number, undefined, true, title, item.account_id)
        : await runsStore.fetchIssue(projectId, repoId, number, item.account_id)
      await conductorStore.start(run.id, checkGoal(await store.loadCheckPrompt(), item), {
        permissionModeOverride: resolvePermissionMode(null, appSettings.settings?.default_permission_mode),
      })
      await store.linkRun(item.key, run.id)
    })
    toast.success(t('githubMentions.check.started', { ref: `${item.repo}#${number}` }))
  } catch (e) {
    toast.error(t('githubMentions.check.failed', { error: String(e) }))
  }
}

// Keyboard: j / k select a row, Enter = "Check with AI" (or open its
// session), o = open on GitHub (marks read), c = copy link.
const scrollEl = ref<HTMLElement | null>(null)
const visibleRows = computed(() => {
  const rows = new Map<string, MentionItem>()
  for (const g of groups.value) {
    if (store.groupBy !== 'none' && collapsed.value.has(g.key)) continue
    for (const item of g.items) rows.set(item.key, item)
  }
  return rows
})
function rowAction(fn: (item: MentionItem) => void | Promise<void>) {
  return (key: string) => {
    const item = visibleRows.value.get(key)
    if (item) void fn(item)
  }
}
const keyboard = useInboxKeyboard({
  keys: () => [...visibleRows.value.keys()],
  root: () => scrollEl.value,
  onPrimary: rowAction(item => {
    if (categoryOf(item) === 'done' && checkStatus(item) === 'none') return
    return check(item)
  }),
  onOpen: rowAction(async item => {
    try {
      await openUrl(item.trigger?.html_url || item.html_url)
      if (item.unread) await store.markRead(item.key)
    } catch (e) {
      toast.error(String(e))
    }
  }),
  onCopy: rowAction(async item => {
    try {
      await navigator.clipboard.writeText(item.trigger?.html_url || item.html_url)
      toast.success(t('githubMentions.menu.copied'))
    } catch (e) {
      toast.error(String(e))
    }
  }),
})

// "+ Todo" from the card menu (Focus §3.1), prefilled from the mention.
const todoOpen = ref(false)
const todoPrefill = ref<TodoPrefill>({})
function addTodo() {
  const item = menu.value.item
  closeMenu()
  if (!item) return
  const repo = `${item.owner}/${item.repo}`
  const refText = item.kind === 'Discussion' ? `${repo} · Discussion` : item.number !== null ? `${repo}#${item.number}` : repo
  todoPrefill.value = {
    text: t('githubMentions.todoText', { title: item.title, ref: refText }),
    projectId: item.project_id ?? undefined,
    sourceUrl: item.trigger?.html_url || item.html_url,
  }
  todoOpen.value = true
}

// Card menu (right-click). Teleported ContextMenu, like the PR Inbox.
const menu = ref<{ open: boolean; x: number; y: number; item: MentionItem | null }>({ open: false, x: 0, y: 0, item: null })
function openMenu(item: MentionItem, x: number, y: number) {
  menu.value = { open: true, x, y, item }
}
function closeMenu() {
  menu.value = { ...menu.value, open: false }
}

const menuMoves = computed<MentionCategory[]>(() => {
  const item = menu.value.item
  return item && !item.answered ? MENTION_CATEGORIES.filter(c => c !== categoryOf(item)) : []
})

async function moveTo(category: MentionCategory | null) {
  const item = menu.value.item
  closeMenu()
  if (!item) return
  try {
    await store.setCategory(item.key, category)
    toast.success(category ? t('githubMentions.menu.moved', { tab: t(`githubMentions.tab.${category}`) }) : t('githubMentions.menu.autoSorted'))
  } catch (e) {
    toast.error(String(e))
  }
}

async function menuAction(action: 'open' | 'read' | 'copy' | 'dismiss' | 'muteThread' | 'muteRepo') {
  const item = menu.value.item
  closeMenu()
  if (!item) return
  const url = item.trigger?.html_url || item.html_url
  try {
    if (action === 'open') {
      await openUrl(url)
      if (item.unread) await store.markRead(item.key)
    }
    else if (action === 'read') await store.markRead(item.key)
    else if (action === 'copy') {
      await navigator.clipboard.writeText(url)
      toast.success(t('githubMentions.menu.copied'))
    } else if (action === 'dismiss') {
      await store.dismiss(item.key)
      toast.success(t('githubMentions.menu.dismissed'))
    } else if (action === 'muteThread') {
      await store.mute(item.key, 'thread')
      toast.success(t('githubMentions.menu.mutedThread'))
    } else {
      await store.mute(item.key, 'repo')
      toast.success(t('githubMentions.menu.mutedRepo', { repo: `${item.owner}/${item.repo}` }))
    }
  } catch (e) {
    toast.error(String(e))
  }
}
</script>

<template>
  <div class="@container/screen flex h-full flex-col">
    <PageHeader
      :title="t('githubMentions.title')"
      :count="store.items.length > 0 ? store.pendingCount : null"
      :count-title="t('githubMentions.pendingTitle')"
    >
      <template #actions>
        <!-- Search · Refresh · settings (same layout as the PR Inbox) -->
        <PageHeaderSearch
          v-if="store.items.length > 0"
          v-model="search"
          :placeholder="t('githubMentions.searchPlaceholder')"
          :clear-label="t('githubMentions.clearSearch')"
        />
        <Button
          variant="outline"
          size="icon"
          :disabled="store.loading"
          :title="t('common.refresh')"
          :aria-label="t('common.refresh')"
          @click="store.refresh()"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="1.75" />
        </Button>
        <MentionOptionsMenu
          :options="options"
          :model-value="store.filters"
          :active-count="activeCount"
          :option-label="optionLabel"
          @update="store.setFilter"
          @clear="store.clearFilters()"
          @edit-prompt="promptEditorOpen = true"
        />
        <DockWindowButton v-if="standalone" surface="mentions" />
      </template>
    </PageHeader>

    <ListFilterSummary :parts="filterParts" class="!px-0 mx-3 pt-2 @[40rem]/screen:mx-6" @clear="store.clearFilters()" />

    <!-- Focusable so the list takes j / k / Enter / o / c (useInboxKeyboard) -->
    <div
      ref="scrollEl"
      class="flex-1 overflow-auto p-3 outline-none @[40rem]/screen:p-6"
      tabindex="0"
      @keydown="keyboard.onKeydown"
    >
      <Banner v-if="store.error" tone="destructive" class="mb-4">{{ store.error }}</Banner>
      <Banner v-if="store.warnings.length > 0" tone="warning" class="mb-4">
        <p class="font-medium text-warning">{{ t('githubMentions.partialFailTitle', store.warnings.length) }}</p>
        <p class="mt-0.5 text-muted-foreground">
          {{ store.warnings.map(w => `${w.account_label}: ${w.message}`).join(' · ') }}
        </p>
      </Banner>

      <div v-if="store.loading && store.items.length === 0" class="flex flex-col gap-2">
        <p class="mb-1 text-xs text-muted-foreground">{{ t('githubMentions.firstLoadHint') }}</p>
        <Skeleton v-for="i in 5" :key="i" class="h-20 rounded-lg" />
      </div>

      <div
        v-else-if="store.items.length === 0 && !store.error"
        class="flex h-full min-h-80 flex-col items-center justify-center text-center"
      >
        <div class="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
          <AtSign class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('githubMentions.emptyTitle') }}</p>
        <p class="mt-1 max-w-72 text-xs text-muted-foreground">{{ t('githubMentions.emptyHint', { days: store.lookbackDays }) }}</p>
      </div>

      <!-- `@container/list`: cards and group headers size themselves against
           the list width, at the same steps as the PR Inbox. -->
      <div v-else class="@container/list flex flex-col gap-5">
        <div
          v-if="visible.length === 0"
          class="flex items-center gap-2 rounded-lg border border-dashed border-border/60 px-3 py-3 text-xs text-muted-foreground"
        >
          <SearchX class="h-4 w-4 shrink-0" :stroke-width="1.75" />
          <span>{{ t('githubMentions.noMatch') }}</span>
          <span class="text-muted-foreground/50">·</span>
          <button type="button" class="cursor-pointer font-medium text-primary hover:underline" @click="search = ''; store.clearFilters()">
            {{ t('githubMentions.clearSearchAndFilters') }}
          </button>
        </div>

        <template v-else>
          <div class="flex flex-wrap items-center gap-3">
            <SegmentedControl
              :model-value="store.tab"
              :options="tabOptions"
              :stretch="false"
              @update:model-value="store.setTab"
            />
            <span v-if="store.triaging > 0" class="inline-flex items-center gap-1.5 text-[11px] text-muted-foreground">
              <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
              {{ t('githubMentions.triaging', store.triaging) }}
            </span>
          </div>

          <p v-if="split[store.tab].length === 0" class="flex items-center gap-2 text-xs text-muted-foreground">
            <Check v-if="store.tab === 'reply'" class="h-4 w-4 text-success" :stroke-width="2" />
            {{ t(`githubMentions.tabEmpty.${store.tab}`, { days: Math.min(7, store.lookbackDays) }) }}
          </p>

          <section v-for="g in groups" :key="g.key" class="flex flex-col gap-2">
            <InboxGroupHeader
              v-if="store.groupBy !== 'none'"
              :label="groupLabel(g)"
              :count="g.items.length"
              :collapsed="collapsed.has(g.key)"
              :mono="store.groupBy === 'repo'"
              @toggle="toggleGroup(g.key)"
            >
              <template #icon>
                <PrAvatar v-if="store.groupBy === 'person'" :url="g.avatarUrl" :name="g.label || '?'" :size="16" />
                <BookMarked v-else-if="store.groupBy === 'repo'" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <CircleDashed v-else-if="g.unmapped" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <Folder v-else class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              </template>
            </InboxGroupHeader>
            <InboxList v-if="!collapsed.has(g.key)">
              <MentionCard
                v-for="(item, i) in g.items"
                :key="item.key"
                :data-inbox-key="item.key"
                :item="item"
                :selected="keyboard.selected.value === item.key"
                :run-status="runStatus(item)"
                :group-by="store.groupBy"
                :show-owner="ambiguousRepos.has(item.repo.toLowerCase())"
                :show-account="showAccount"
                :account-tone="accountTone"
                :index="i"
                :check-status="checkStatus(item)"
                :starting="store.busyKeys.has(item.key)"
                @select="keyboard.select(item.key)"
                @menu="openMenu"
                @check="check"
              />
            </InboxList>
          </section>
        </template>
      </div>
    </div>

    <ContextMenu :open="menu.open" :x="menu.x" :y="menu.y" @close="closeMenu">
      <DropdownItem @click="menuAction('open')">
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.openOnGithub') }}
      </DropdownItem>
      <DropdownItem v-if="menu.item?.unread" @click="menuAction('read')">
        <MailOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.markRead') }}
      </DropdownItem>
      <DropdownItem @click="menuAction('copy')">
        <Copy class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.copyLink') }}
      </DropdownItem>
      <DropdownItem v-if="menu.item && categoryOf(menu.item) !== 'done'" @click="addTodo">
        <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('focus.addTodoHint') }}
      </DropdownItem>
      <template v-if="menuMoves.length > 0">
        <DropdownSeparator />
        <DropdownItem v-for="c in menuMoves" :key="c" @click="moveTo(c)">
          <component :is="CATEGORY_ICONS[c]" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('githubMentions.menu.moveTo', { tab: t(`githubMentions.tab.${c}`) }) }}
        </DropdownItem>
        <DropdownItem v-if="menu.item?.category_source === 'user'" @click="moveTo(null)">
          <Undo2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('githubMentions.menu.autoSort') }}
        </DropdownItem>
      </template>
      <DropdownSeparator />
      <DropdownItem @click="menuAction('dismiss')">
        <EyeOff class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.dismiss') }}
      </DropdownItem>
      <DropdownItem @click="menuAction('muteThread')">
        <BellOff class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.muteThread') }}
      </DropdownItem>
      <DropdownItem @click="menuAction('muteRepo')">
        <FolderX class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.muteRepo', { repo: menu.item ? `${menu.item.owner}/${menu.item.repo}` : '' }) }}
      </DropdownItem>
    </ContextMenu>
    <TodoCaptureModal :open="todoOpen" :prefill="todoPrefill" @close="todoOpen = false" />
    <PromptConfigModal
      :open="promptEditorOpen"
      :title="t('githubMentions.prompt.title')"
      :hint="t('githubMentions.prompt.hint')"
      :load="store.loadPromptConfig"
      :save-text="store.savePrompt"
      :save-id="store.savePromptId"
      @close="promptEditorOpen = false"
    />
  </div>
</template>
