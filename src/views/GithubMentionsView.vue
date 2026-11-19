<script setup lang="ts">
// GitHub Mentions: comments on GitHub that @-mention the user directly and
// wait for their answer, across all linked GitHub accounts. Header layout follows the PR Inbox:
// search + Filters / Display beside Refresh. Mentions are split into tabs by
// what they need: "Needs reply" (the badge), "FYI" and "Done" (answered in the
// last week, closed / merged, acknowledged) — sorted by the backend's rules
// and AI, movable by hand from the card menu. "Check with AI" opens a
// session on the issue / PR (like the PR Inbox's AI review), starting with the
// configurable check prompt; the user answers on GitHub themselves.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import { emit as emitEvent } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Banner, Button, ContextMenu, DropdownItem, DropdownSeparator, Input, PageHeader, SegmentedControl, Skeleton } from '@/components/ui'
import MentionCard from '@/components/githubMentions/MentionCard.vue'
import MentionFiltersPopover from '@/components/githubMentions/MentionFiltersPopover.vue'
import MentionDisplayPopover from '@/components/githubMentions/MentionDisplayPopover.vue'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import PromptConfigModal from '@/components/PromptConfigModal.vue'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { useToast } from '@/composables/useToast'
import { useRunsStore } from '@/stores/runs'
import { useConductorStore } from '@/stores/conductor'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useLiveRunsStore } from '@/stores/liveRuns'
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
  type MentionItem,
} from '@/lib/githubMentions'
import {
  AtSign, BellOff, Check, CheckCheck, ChevronRight, Copy, EyeOff, ExternalLink, FolderX, Info, Loader2, MailOpen, RefreshCw, Reply,
  Search, SearchX, Undo2, X,
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

const search = ref('')
const collapsed = ref<Set<string>>(new Set())

onMounted(() => {
  void store.load()
  void store.loadMutes()
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

const chips = computed(() =>
  (Object.keys(effective.value) as MentionFilterDimension[]).flatMap(d =>
    effective.value[d].map(value => {
      const o = options.value[d].find(x => x.value === value)
      return { key: `${d}:${value}`, dimension: d, value, title: t(`githubMentions.filters.${d === 'reasons' ? 'reason' : d === 'accounts' ? 'account' : 'repo'}`), label: o ? optionLabel(d, o) : value }
    }),
  ),
)

function removeChip(d: MentionFilterDimension, value: string) {
  store.setFilter(d, store.filters[d].filter(v => v !== value))
}

function toggleGroup(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
}

// ── Check with AI ────────────────────────────────────────────────────────────
// Live status (the liveRuns store) wins over the snapshot taken at fetch time.
function checkStatus(item: MentionItem) {
  const id = item.existing_run_id
  const liveStatus = !id ? null : windowRuns ? windowRuns.status(id) : live.get(id)?.status
  const runStatus = id ? (liveStatus ?? item.existing_run_status) : null
  return checkStatusOf(item, runStatus)
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
    if (action === 'open') await openUrl(url)
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
  <div class="flex h-full flex-col">
    <PageHeader
      :title="t('githubMentions.title')"
      :count="store.items.length > 0 ? store.pendingCount : null"
      :count-title="t('githubMentions.pendingTitle')"
    >
      <template #leading>
        <AtSign class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
      </template>
      <template #actions>
        <template v-if="store.items.length > 0">
          <div class="relative">
            <Search class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
            <Input
              v-model="search"
              size="sm"
              class="w-56 pl-7 pr-7"
              :placeholder="t('githubMentions.searchPlaceholder')"
              :aria-label="t('githubMentions.searchPlaceholder')"
              @keydown.esc="search = ''"
            />
            <button
              v-if="search"
              type="button"
              class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
              :title="t('githubMentions.clearSearch')"
              :aria-label="t('githubMentions.clearSearch')"
              @click="search = ''"
            >
              <X class="h-3.5 w-3.5" :stroke-width="2" />
            </button>
          </div>
          <MentionFiltersPopover
            :options="options"
            :model-value="store.filters"
            :active-count="activeCount"
            :option-label="optionLabel"
            @update="store.setFilter"
            @clear="store.clearFilters()"
          />
          <MentionDisplayPopover @edit-prompt="promptEditorOpen = true" />
        </template>
        <Button variant="outline" :disabled="store.loading" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="1.75" />
          {{ t('common.refresh') }}
        </Button>
      </template>
    </PageHeader>

    <div v-if="chips.length > 0" class="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-border/60 px-6 py-2">
      <span
        v-for="chip in chips"
        :key="chip.key"
        class="inline-flex max-w-full items-center gap-1 rounded-md bg-muted py-1 pl-2 pr-1 text-[11px] text-foreground"
      >
        <span class="shrink-0 text-muted-foreground">{{ chip.title }}:</span>
        <span class="min-w-0 max-w-40 truncate" :title="chip.label">{{ chip.label }}</span>
        <button
          type="button"
          class="shrink-0 rounded p-0.5 cursor-pointer text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          :title="t('githubMentions.filters.remove', { filter: `${chip.title}: ${chip.label}` })"
          :aria-label="t('githubMentions.filters.remove', { filter: `${chip.title}: ${chip.label}` })"
          @click="removeChip(chip.dimension, chip.value)"
        >
          <X class="h-3 w-3" :stroke-width="2" />
        </button>
      </span>
      <button type="button" class="ml-1 cursor-pointer text-[11px] font-medium text-primary hover:underline" @click="store.clearFilters()">
        {{ t('githubMentions.filters.clearAll') }}
      </button>
    </div>

    <div class="flex-1 overflow-auto p-6">
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

      <div v-else class="flex flex-col gap-5">
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
            <button
              v-if="store.groupBy !== 'none'"
              type="button"
              class="flex min-w-0 items-center gap-2 text-left text-xs font-semibold cursor-pointer"
              :aria-expanded="!collapsed.has(g.key)"
              @click="toggleGroup(g.key)"
            >
              <ChevronRight
                class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
                :class="{ 'rotate-90': !collapsed.has(g.key) }"
                :stroke-width="2"
              />
              <PrAvatar v-if="store.groupBy === 'person'" :url="g.avatarUrl" :name="g.label || '?'" :size="16" />
              <span class="min-w-0 truncate" :class="{ 'font-mono font-medium': store.groupBy === 'repo' }">{{ g.label || t('githubMentions.unknownAuthor') }}</span>
              <span class="shrink-0 font-normal text-[11px] text-muted-foreground">{{ g.items.length }}</span>
            </button>
            <template v-if="!collapsed.has(g.key)">
              <MentionCard
                v-for="item in g.items"
                :key="item.key"
                :item="item"
                :check-status="checkStatus(item)"
                :starting="store.busyKeys.has(item.key)"
                @menu="openMenu"
                @check="check"
              />
            </template>
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
