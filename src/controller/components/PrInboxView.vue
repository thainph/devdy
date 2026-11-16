<script setup lang="ts">
/**
 * PR tab of the phone Controller: the desktop PR Inbox — the PRs waiting for
 * the user's review — with a search, a "hide bots" switch and, per PR, the
 * desktop's Review / Re-review actions. Tapping a PR that has a review session
 * opens it.
 *
 * Backend-agnostic like the other controller views: the list comes in as a
 * prop (the review run's live status from the run list); actions go out as
 * events. The project filter is the tab header's dropdown (`projectId`).
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ExternalLink, Loader2, RefreshCw, X } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import { relativeAge } from '@/lib/itemText'
import {
  botCount,
  filterPrs,
  prAction,
  prReviewState,
  reviewRunStatus,
  type PrAction,
  type PrReviewState,
} from '@/lib/remotePrInbox'
import SearchField from './SearchField.vue'
import type { PrInboxInfo, RunInfo } from '../protocol'

const props = defineProps<{
  /** null until the Host's first `pr_inbox`. */
  items: PrInboxInfo[] | null
  /** The run list: the review sessions' live status. */
  runs: RunInfo[]
  /** The shared project scope (null = all). */
  projectId: string | null
  refreshing: boolean
  fetchedAt: string | null
  failedAccounts: string[]
  /** The Host's last fetch failed (code). */
  fetchError: string | null
  /** The list request failed (refused / no answer). */
  failed?: boolean
  /** PR keys with a review action in flight. */
  busy: Record<string, true>
  /** The last refused action, already translated. */
  error: string | null
}>()

const emit = defineEmits<{
  refresh: []
  retry: []
  review: [key: string]
  'review-again': [key: string]
  /** Open a review session. */
  open: [runId: string]
  'clear-error': []
}>()

const { t } = useI18n()

const query = ref('')

// Hiding bots is a reading choice of this phone, like the desktop's.
const HIDE_BOTS_KEY = 'devdy.controller.prInbox.hideBots'
function loadHideBots(): boolean {
  try {
    return localStorage.getItem(HIDE_BOTS_KEY) !== '0'
  } catch {
    return true
  }
}
const hideBots = ref(loadHideBots())
watch(hideBots, (on) => {
  try {
    localStorage.setItem(HIDE_BOTS_KEY, on ? '1' : '0')
  } catch {
    // Storage unavailable: the choice lasts for this visit only.
  }
})

const shown = computed(() =>
  filterPrs(props.items ?? [], { query: query.value, hideBots: hideBots.value, projectId: props.projectId }),
)
const bots = computed(() =>
  botCount((props.items ?? []).filter((pr) => props.projectId === null || pr.project_id === props.projectId)),
)
const searching = computed(() => query.value.trim().length > 0)

interface Row {
  pr: PrInboxInfo
  state: PrReviewState
  action: PrAction
}
const rows = computed<Row[]>(() =>
  shown.value.map((pr) => {
    const state = prReviewState(pr, reviewRunStatus(pr, props.runs))
    return { pr, state, action: prAction(state, !!props.busy[pr.key]) }
  }),
)

const STATE_TONES: Record<PrReviewState, string> = {
  unmapped: 'border-border bg-secondary text-muted-foreground',
  not_reviewed: 'border-primary/40 bg-primary/10 text-primary',
  running: 'border-warning/40 bg-warning/10 text-warning',
  reviewed: 'border-success/40 bg-success/10 text-success',
  updated: 'border-destructive/40 bg-destructive/10 text-destructive',
}
const CHIP_MUTED = 'border-border bg-secondary text-muted-foreground'

function onCard(row: Row): void {
  if (row.pr.existing_run_id) emit('open', row.pr.existing_run_id)
}

function onAction(row: Row): void {
  if (row.action === 'review') emit('review', row.pr.key)
  else if (row.action === 'review_again') emit('review-again', row.pr.key)
  else if (row.action === 'view' && row.pr.existing_run_id) emit('open', row.pr.existing_run_id)
}

function openOnGitHub(pr: PrInboxInfo): void {
  if (/^https:\/\//.test(pr.html_url)) window.open(pr.html_url, '_blank', 'noopener')
}
</script>

<template>
  <div class="relative flex h-full min-h-0 flex-col text-foreground">
    <Banner
      v-if="error"
      tone="destructive"
      class="shrink-0 gap-2 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{ error }}
      <template #actions>
        <button
          class="shrink-0 cursor-pointer text-destructive"
          :title="t('controller.data.dismiss')"
          :aria-label="t('controller.data.dismiss')"
          @click="emit('clear-error')"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </template>
    </Banner>
    <Banner
      v-else-if="fetchError || failedAccounts.length"
      tone="warning"
      class="shrink-0 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{
        fetchError
          ? t('controller.prs.fetchFailed')
          : t('controller.prs.accountsFailed', { accounts: failedAccounts.join(', ') })
      }}
    </Banner>

    <SearchField
      v-model="query"
      :placeholder="t('controller.prs.searchPlaceholder')"
      :clear-label="t('controller.prs.clearSearch')"
    />

    <div class="flex shrink-0 items-center gap-2 border-b border-border/40 px-3 py-1.5 text-[11px] text-muted-foreground">
      <label class="flex cursor-pointer items-center gap-1.5">
        <input v-model="hideBots" type="checkbox" class="h-3.5 w-3.5 accent-primary">
        {{ t('controller.prs.hideBots', { count: bots }) }}
      </label>
      <span class="ml-auto truncate">
        {{
          refreshing
            ? t('controller.prs.refreshing')
            : fetchedAt
              ? t('controller.prs.fetchedAt', { age: relativeAge(fetchedAt) })
              : ''
        }}
      </span>
      <Button
        variant="ghost"
        size="icon-sm"
        :disabled="refreshing"
        :title="t('controller.prs.refresh')"
        :aria-label="t('controller.prs.refresh')"
        @click="emit('refresh')"
      >
        <RefreshCw class="h-3.5 w-3.5" :class="refreshing && 'animate-spin'" :stroke-width="2" />
      </Button>
    </div>

    <main class="flex-1 min-h-0 overflow-auto px-3 pt-3 pb-6">
      <div v-if="items === null && failed" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <Button variant="outline" @click="emit('retry')">
          {{ t('controller.data.retry') }}
        </Button>
      </div>
      <p
        v-else-if="items === null || (!items.length && refreshing && !fetchedAt)"
        class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50"
      >
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>
      <p v-else-if="!rows.length" class="py-10 text-center text-sm text-foreground/50">
        {{ searching ? t('controller.prs.noResults') : t('controller.prs.empty') }}
      </p>

      <div v-else class="ui-card overflow-hidden rounded-xl border border-border bg-card">
        <div
          v-for="row in rows"
          :key="row.pr.key"
          class="px-3 py-2.5 border-b border-border/60 last:border-b-0"
          :class="row.pr.existing_run_id && 'cursor-pointer hover:bg-accent/50'"
          @click="onCard(row)"
        >
          <div class="flex items-baseline gap-2 text-[11px] text-muted-foreground">
            <span class="min-w-0 flex-1 truncate">{{ row.pr.owner }}/{{ row.pr.repo }} #{{ row.pr.number }}</span>
            <span class="shrink-0">{{ relativeAge(row.pr.updated_at) }}</span>
            <button
              v-if="row.pr.html_url"
              class="shrink-0 self-center cursor-pointer text-muted-foreground hover:text-foreground"
              :title="t('controller.prs.openOnGitHub')"
              :aria-label="t('controller.prs.openOnGitHub')"
              @click.stop="openOnGitHub(row.pr)"
            >
              <ExternalLink class="h-3.5 w-3.5" :stroke-width="2" />
            </button>
          </div>
          <p class="mt-0.5 line-clamp-2 text-sm font-medium break-words">{{ row.pr.title }}</p>

          <div class="mt-1.5 flex items-end gap-2">
            <div class="flex min-w-0 flex-1 flex-wrap items-center gap-1">
              <span class="rounded-full border px-1.5 py-px text-[10px]" :class="STATE_TONES[row.state]">
                {{ t(`controller.prs.state.${row.state}`) }}
              </span>
              <span v-if="row.pr.draft" class="rounded-full border px-1.5 py-px text-[10px]" :class="CHIP_MUTED">
                {{ t('controller.prs.draft') }}
              </span>
              <span
                v-if="row.pr.project_name"
                class="max-w-40 truncate rounded-full border px-1.5 py-px text-[10px]"
                :class="CHIP_MUTED"
              >
                {{ row.pr.project_name }}
              </span>
              <span class="truncate text-[11px] text-muted-foreground">
                @{{ row.pr.author_login }}<template v-if="row.pr.author_is_bot"> · bot</template>
              </span>
            </div>
            <Button
              v-if="row.action !== 'none'"
              :variant="row.action === 'review' || (row.action === 'review_again' && row.state === 'updated') ? 'primary' : 'outline'"
              size="xs"
              class="shrink-0"
              :disabled="row.action === 'starting'"
              @click.stop="onAction(row)"
            >
              <Loader2 v-if="row.action === 'starting'" class="h-3 w-3 animate-spin" :stroke-width="2" />
              {{ t(`controller.prs.action.${row.action}`) }}
            </Button>
          </div>
          <p v-if="row.state === 'unmapped'" class="mt-1 text-[11px] text-muted-foreground">
            {{ t('controller.prs.unmappedHint') }}
          </p>
        </div>
      </div>
    </main>
  </div>
</template>
