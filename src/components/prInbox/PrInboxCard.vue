<script setup lang="ts">
// One PR in the inbox (PrInboxView), on the shared InboxCard layout (same as
// GitHub Mentions). `comfortable` is the full two-line card; `compact`
// squeezes it into a single row: avatar · title · repo#num · author · time ·
// action. InboxCard renders the shared parts (kind, repo#num, time, project,
// account); this adds author, labels and other reviewers. Review / navigation stay in the view — this only renders and emits,
// so both densities share one action path.
//
// Context-aware: whatever the current group header already says (project,
// repo, author, reviewer account — `groupBy`) is not repeated on the card.
//
// Responsive: sized by the `@container/list` around the PR list (PrInboxView),
// so it reacts to the content pane, not the window. As the list narrows the
// meta row sheds, in order: other reviewers (< 52rem) → labels (< 44rem) →
// time (< 38rem; a stale time is never hidden) → author (< 36rem). Title and
// the icon-only action button always stay. Only the primary action (AI review / view review) is a button; every
// other action (copy link, re-review, + Todo, add to devdy, bot marking) lives
// in the right-click menu owned by the view.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import type { PrInboxItem } from '@/stores/prInbox'
import type { PrGroupBy } from '@/lib/prInboxGrouping'
import { isStale, reviewerAccountsOf, reviewStatusOf, STALE_AFTER_DAYS } from '@/lib/prInboxFilters'
import { Badge } from '@/components/ui'
import InboxCard, { type InboxCardAccount } from '@/components/inbox/InboxCard.vue'
import InboxActionButton from '@/components/inbox/InboxActionButton.vue'
import PrAvatar from './PrAvatar.vue'
import { User, Users, Bot, Tag, Check, Loader2, RotateCcw } from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  item: PrInboxItem
  density: 'comfortable' | 'compact'
  /** Dimension the surrounding group header already shows ('none' = show all). */
  groupBy?: PrGroupBy
  /** Prefix the repo with `owner/` (its bare name is ambiguous in the inbox). */
  showOwner?: boolean
  /** The user has several GitHub accounts → show which one reviews this PR. */
  showAccount?: boolean
  /** `bg-*` dot class for an account id (stable per configured account). */
  accountTone?: (accountId: string) => string
  /** Keyboard selection (j / k). */
  selected?: boolean
  /** Backend bot flag OR the user's custom bot list. */
  isBot: boolean
  /** Effective status of the linked review run (live wins over DB snapshot). */
  runStatus: string | null
  /** A review run is being created for this PR right now. */
  reviewing: boolean
  /** Epoch ms used for relative / stale time (passed in so all cards agree). */
  now: number
  index?: number
}>(), { groupBy: 'none', showOwner: false, showAccount: false, accountTone: () => 'bg-muted-foreground', selected: false, index: 0 })

const emit = defineEmits<{
  select: [item: PrInboxItem]
  review: [item: PrInboxItem]
  'open-run': [item: PrInboxItem]
  /** Open the card menu at a viewport point (right-click). */
  menu: [item: PrInboxItem, x: number, y: number]
}>()

const { t, te } = useI18n()

const compact = computed(() => props.density === 'compact')

// What the group header already shows is hidden here.
const showProject = computed(() => props.item.mapped && props.groupBy !== 'project')
const showRepoName = computed(() => props.groupBy !== 'repo')
const showAuthor = computed(() => props.groupBy !== 'author')
const accounts = computed(() => reviewerAccountsOf(props.item))
const showAccountChip = computed(() =>
  props.showAccount && props.item.mapped && props.groupBy !== 'reviewer_account',
)


const stale = computed(() => isStale(props.item.updated_at, props.now))
const labels = computed(() => props.item.labels ?? [])
const labelsTitle = computed(() => t('prInbox.labelsTitle', { list: labels.value.join(', ') }))

// Other pending reviewers: up to 3 overlapping avatars + "+N".
const reviewers = computed(() => props.item.requested_reviewers ?? [])
const shownReviewers = computed(() => reviewers.value.slice(0, 3))
// Teams arrive as `org/slug` (GitHub's combinedSlug) — same `@` form as GitHub.
const reviewersTitle = computed(() =>
  t('prInbox.reviewersTitle', { list: reviewers.value.map(r => `@${r.login_or_slug}`).join(', ') }),
)

// Account the review runs as: stable colour dot (+N when several of the
// user's accounts were asked).
const account = computed<InboxCardAccount | null>(() => {
  const [primary, ...others] = accounts.value
  if (!showAccountChip.value || !primary) return null
  return {
    label: primary.account_label,
    tone: props.accountTone(primary.account_id),
    more: others.length,
    title: others.length === 0
      ? t('prInbox.reviewAs', { account: primary.account_label })
      : t('prInbox.reviewAsMulti', { account: primary.account_label, others: others.map(a => a.account_label).join(', ') }),
  }
})

// The action button carries the review state (no separate status pill):
// none → "AI review", running → "Reviewing", else "View review" + status.
const running = computed(() => props.runStatus === 'running' || props.runStatus === 'fetched')
const statusText = computed(() => {
  if (!props.runStatus) return ''
  const key = `common.status.${props.runStatus}`
  return te(key) ? t(key) : props.runStatus
})
const runLabel = computed(() => (running.value ? t('prInbox.reviewingShort') : t('prInbox.viewReview')))
const runTitle = computed(() => (statusText.value ? `${runLabel.value} · ${statusText.value}` : runLabel.value))
const reviewLabel = computed(() => (props.reviewing ? t('prInbox.opening') : t('prInbox.aiReview')))
// Review status chip, same place on every row (unmapped PRs have none — their
// group says so). "Updated" = new commits since the review.
const reviewStatus = computed(() => reviewStatusOf(props.item, props.runStatus))
const STATUS_CHIP: Record<string, string> = {
  not_reviewed: 'bg-muted text-muted-foreground',
  running: 'bg-info/10 text-info',
  updated: 'bg-warning/10 text-warning',
  reviewed: 'bg-success/10 text-success',
}

const actionState = computed(() =>
  props.item.existing_run_id ? (running.value ? 'running' : 'done') : props.reviewing ? 'starting' : 'none',
)
const actionTitle = computed(() => (props.item.existing_run_id ? runTitle.value : reviewLabel.value))

function onAction() {
  if (props.item.existing_run_id) emit('open-run', props.item)
  else emit('review', props.item)
}

function openPr() {
  openUrl(props.item.html_url).catch(() => { /* opener unavailable */ })
}

function relativeTime(iso: string): string {
  const then = new Date(iso).getTime()
  const diff = props.now - then
  const mins = Math.round(diff / 60000)
  if (mins < 1) return t('prInbox.justNow')
  if (mins < 60) return t('prInbox.minsAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('prInbox.hoursAgo', { hours })
  const days = Math.round(hours / 24)
  if (days < 30) return t('prInbox.daysAgo', { days })
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}
</script>

<template>
  <InboxCard
    :title="item.title"
    kind="PullRequest"
    :owner="item.owner"
    :repo="item.repo"
    :number="item.number"
    :show-owner="showOwner"
    :show-repo="showRepoName"
    :time="relativeTime(item.updated_at)"
    :stale="stale"
    :stale-title="t('prInbox.staleTitle', { days: STALE_AFTER_DAYS })"
    :project-name="showProject ? item.project_name : null"
    :account="account"
    :compact="compact"
    :selected="selected"
    :index="index"
    @select="emit('select', item)"
    @open="openPr"
    @menu="(x, y) => emit('menu', item, x, y)"
  >
    <!-- Author avatar: dimmed for drafts, Bot mark in the corner for bots -->
    <template #avatar>
      <span class="relative block" :class="{ 'opacity-50': item.draft }" :title="item.author_login">
        <PrAvatar :url="item.author_avatar_url" :name="item.author_login" :size="compact ? 20 : 32" />
        <span
          v-if="isBot"
          class="absolute -bottom-0.5 -right-1 flex items-center justify-center rounded-full bg-violet-600 text-white ring-2 ring-card"
          :class="compact ? 'h-3 w-3' : 'h-4 w-4'"
          :title="t('prInbox.bot')"
        >
          <Bot :class="compact ? 'h-2 w-2' : 'h-2.5 w-2.5'" :stroke-width="2.5" aria-hidden="true" />
          <!-- PrAvatar is aria-hidden; this is the only bot cue for screen readers -->
          <span class="sr-only">{{ t('prInbox.bot') }}</span>
        </span>
      </span>
    </template>

    <template #badges>
      <Badge v-if="item.draft" tone="neutral" size="xs" class="shrink-0">{{ t('prInbox.draft') }}</Badge>
    </template>

    <!-- After repo#num: review status · author · labels · other reviewers -->
    <template #meta>
      <span
        v-if="reviewStatus !== 'unmapped'"
        class="inline-flex shrink-0 items-center gap-1 rounded-[5px] px-1.5 py-0.5 text-[10px] font-medium"
        :class="STATUS_CHIP[reviewStatus]"
      >
        <Loader2 v-if="reviewStatus === 'running'" class="h-3 w-3 animate-spin" :stroke-width="2" />
        <RotateCcw v-else-if="reviewStatus === 'updated'" class="h-3 w-3" :stroke-width="2" />
        <Check v-else-if="reviewStatus === 'reviewed'" class="h-3 w-3" :stroke-width="2" />
        {{ t(`prInbox.status.${reviewStatus}`) }}
      </span>
      <span v-if="showAuthor" class="hidden @[36rem]/list:flex min-w-0 items-center gap-1">
        <User class="h-3 w-3 shrink-0" :stroke-width="1.75" />
        <span class="truncate">{{ item.author_login }}</span>
      </span>
      <span
        v-if="labels.length > 0"
        role="img"
        class="hidden @[44rem]/list:flex shrink-0 items-center gap-0.5"
        :title="labelsTitle"
        :aria-label="labelsTitle"
      >
        <Tag class="h-3 w-3" :stroke-width="1.75" />
        {{ labels.length }}
      </span>
      <span
        v-if="reviewers.length > 0"
        role="img"
        class="hidden @[52rem]/list:flex shrink-0 items-center gap-1"
        :title="reviewersTitle"
        :aria-label="reviewersTitle"
      >
        <template v-if="compact">
          <Users class="h-3 w-3" :stroke-width="1.75" />
          {{ reviewers.length }}
        </template>
        <template v-else>
          <span class="flex -space-x-1.5">
            <span
              v-for="r in shownReviewers"
              :key="`${r.kind}:${r.login_or_slug}`"
              class="rounded-full ring-2 ring-card"
            >
              <PrAvatar v-if="r.kind === 'user'" :url="r.avatar_url" :name="r.login_or_slug" :size="16" />
              <span v-else class="flex h-4 w-4 items-center justify-center rounded-full bg-muted text-muted-foreground">
                <Users class="h-2.5 w-2.5" :stroke-width="2" />
              </span>
            </span>
          </span>
          <span v-if="reviewers.length > shownReviewers.length" class="text-[10px]">
            +{{ reviewers.length - shownReviewers.length }}
          </span>
        </template>
      </span>
    </template>

    <!-- Unmapped PRs have no action — "Add to devdy" is in the menu -->
    <template v-if="item.mapped" #action>
      <InboxActionButton
        :state="actionState"
        :title="actionTitle"
        :run-status="runStatus"
        :compact="compact"
        @click="onAction"
      />
    </template>
  </InboxCard>
</template>
