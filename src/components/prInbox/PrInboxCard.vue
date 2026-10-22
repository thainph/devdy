<script setup lang="ts">
// One PR in the inbox (PrInboxView). `comfortable` is the full two-line card;
// `compact` squeezes it into a single row: avatar · title · repo#num · author ·
// time · action. Review / navigation stay in the view — this only renders and
// emits, so both densities share one action path.
//
// Context-aware: whatever the current group header already says (project,
// repo, author, reviewer account — `groupBy`) is not repeated on the card.
//
// Responsive: sized by the `@container/list` around the PR list (PrInboxView),
// so it reacts to the content pane, not the window. As the list narrows the
// meta row sheds, in order: other reviewers (< 52rem) → labels (< 44rem) →
// time (< 38rem; a stale time is never hidden) → author (< 36rem). Title and
// the action button always stay (the button goes icon-only, title + aria-label
// kept).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import type { PrInboxItem } from '@/stores/prInbox'
import type { PrGroupBy } from '@/lib/prInboxGrouping'
import { isStale, reviewerAccountsOf, STALE_AFTER_DAYS } from '@/lib/prInboxFilters'
import { runStatusDotClass } from '@/lib/runDisplay'
import { Button, Card, Badge } from '@/components/ui'
import PrAvatar from './PrAvatar.vue'
import {
  RefreshCw, Sparkles, FolderPlus, ExternalLink, User, Users, Eye, Bot, MoreHorizontal, Tag,
} from 'lucide-vue-next'

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
  /** Inside a group header (slightly tighter padding than the flat list). */
  grouped?: boolean
  /** Backend bot flag OR the user's custom bot list. */
  isBot: boolean
  /** Effective status of the linked review run (live wins over DB snapshot). */
  runStatus: string | null
  /** A review run is being created for this PR right now. */
  reviewing: boolean
  /** Epoch ms used for relative / stale time (passed in so all cards agree). */
  now: number
  index?: number
}>(), { groupBy: 'none', showOwner: false, showAccount: false, accountTone: () => 'bg-muted-foreground', grouped: false, index: 0 })

const emit = defineEmits<{
  review: [item: PrInboxItem]
  'open-run': [item: PrInboxItem]
  /** "Add to devdy" on an unmapped PR — the host decides how to navigate
   * (router in the main window, a request to it from the PR Inbox window). */
  'add-to-devdy': []
  /** Open the card menu at a viewport point (⋯ button or right-click). */
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

const fullRef = computed(() => `${props.item.owner}/${props.item.repo}#${props.item.number}`)
const repoText = computed(() => (props.showOwner ? `${props.item.owner}/${props.item.repo}` : props.item.repo))

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

const accountTitle = computed(() => {
  const [primary, ...others] = accounts.value
  if (!primary) return ''
  return others.length === 0
    ? t('prInbox.reviewAs', { account: primary.account_label })
    : t('prInbox.reviewAsMulti', { account: primary.account_label, others: others.map(a => a.account_label).join(', ') })
})

// The action button carries the review state (no separate status pill):
// none → "AI review", running → "Reviewing" + pulsing dot, else "View review"
// + a dot in the run-status colour (runDisplay mirrors StatusBadge's palette).
const running = computed(() => props.runStatus === 'running' || props.runStatus === 'fetched')
const statusText = computed(() => {
  if (!props.runStatus) return ''
  const key = `common.status.${props.runStatus}`
  return te(key) ? t(key) : props.runStatus
})
const runLabel = computed(() => (running.value ? t('prInbox.reviewingShort') : t('prInbox.viewReview')))
const runTitle = computed(() => (statusText.value ? `${runLabel.value} · ${statusText.value}` : runLabel.value))
const reviewLabel = computed(() => (props.reviewing ? t('prInbox.opening') : t('prInbox.aiReview')))
// Action labels show from this container width up; below it, icon-only.
const actionLabelClass = computed(() => (compact.value ? 'hidden @[44rem]/list:inline' : 'hidden @[36rem]/list:inline'))

function openPr() {
  openUrl(props.item.html_url).catch(() => { /* opener unavailable */ })
}

function openMenuFromButton(e: MouseEvent) {
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect()
  emit('menu', props.item, rect.left, rect.bottom + 4)
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
  <Card
    class="group relative flex flex-row items-center animate-fade-rise transition-all duration-150 hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)]"
    :style="{ animationDelay: `${Math.min(index, 12) * 40}ms` }"
    :body-class="`flex flex-row items-center flex-1 min-w-0 ${compact ? 'gap-2.5 px-3 py-1.5' : grouped ? 'gap-3 p-3' : 'gap-3 p-4'}`"
    @contextmenu.prevent="emit('menu', item, $event.clientX, $event.clientY)"
  >
    <!-- Author avatar: dimmed for drafts, Bot mark in the corner for bots -->
    <span class="relative shrink-0" :class="{ 'opacity-50': item.draft }" :title="item.author_login">
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

    <!-- Main info. Compact: title block grows (floor min-w-24), meta block is
         capped and truncates, so neither collapses nor overlaps the actions. -->
    <div class="min-w-0 flex-1" :class="{ 'flex items-center gap-2': compact }">
      <div class="flex items-center gap-1.5 min-w-0" :class="{ 'flex-1 min-w-24': compact }">
        <button
          class="min-w-0 font-semibold truncate leading-tight hover:text-primary transition-colors cursor-pointer text-left"
          :class="compact ? 'text-xs' : 'text-sm'"
          :title="item.title"
          @click="openPr"
        >
          {{ item.title }}
        </button>
        <ExternalLink class="h-3 w-3 shrink-0 text-muted-foreground/50 opacity-0 group-hover:opacity-100 transition-opacity" :stroke-width="2" />
        <Badge v-if="item.draft" tone="neutral" size="xs" class="shrink-0">{{ t('prInbox.draft') }}</Badge>
      </div>

      <!-- Meta: at most repo#num · project (narrow) · author · time · labels · reviewers -->
      <div
        class="flex min-w-0 items-center gap-2 text-[11px] text-muted-foreground"
        :class="compact ? 'max-w-[45%]' : 'mt-1'"
      >
        <span class="min-w-0 font-mono truncate" :title="fullRef">
          <template v-if="showRepoName">{{ repoText }} </template><span class="text-muted-foreground/70">#{{ item.number }}</span>
        </span>
        <!-- Project moves here from the right side on narrow containers -->
        <Badge
          v-if="!compact && showProject"
          tone="primary"
          size="xs"
          class="shrink-0 max-w-32 truncate @[44rem]/list:hidden"
          :title="item.project_name ?? ''"
        >
          {{ item.project_name }}
        </Badge>
        <span v-if="showAuthor" class="hidden @[36rem]/list:flex min-w-0 items-center gap-1">
          <User class="h-3 w-3 shrink-0" :stroke-width="1.75" />
          <span class="truncate">{{ item.author_login }}</span>
        </span>
        <!-- Time: hidden on narrow containers unless stale — staleness is a
             signal worth the space. sr-only text so it isn't colour-only. -->
        <span
          class="shrink-0"
          :class="stale ? 'text-amber-600 dark:text-amber-400' : 'hidden @[38rem]/list:inline text-muted-foreground/60'"
          :title="stale ? t('prInbox.staleTitle', { days: STALE_AFTER_DAYS }) : undefined"
        >
          {{ relativeTime(item.updated_at) }}
          <span v-if="stale" class="sr-only">({{ t('prInbox.staleTitle', { days: STALE_AFTER_DAYS }) }})</span>
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
      </div>
    </div>

    <!-- Right side: project (wide) · review-as account · action · ⋯ -->
    <div class="flex items-center gap-2 shrink-0">
      <template v-if="item.mapped">
        <Badge
          v-if="!compact && showProject"
          tone="primary"
          size="xs"
          class="hidden @[44rem]/list:inline-flex shrink-0 max-w-40 truncate"
          :title="item.project_name ?? ''"
        >
          {{ item.project_name }}
        </Badge>
        <!-- Account the review runs as: stable colour dot (+N when several of
             the user's accounts were asked) -->
        <span
          v-if="showAccountChip && accounts.length > 0"
          class="flex shrink-0 items-center gap-1 text-[11px] text-muted-foreground"
          :title="accountTitle"
          :aria-label="accountTitle"
          role="img"
        >
          <span class="h-2 w-2 shrink-0 rounded-full" :class="accountTone(accounts[0].account_id)" />
          <span class="max-w-24 truncate" :class="compact ? 'hidden' : 'hidden @[44rem]/list:inline'">
            {{ accounts[0].account_label }}
          </span>
          <span v-if="accounts.length > 1" class="text-[10px]">+{{ accounts.length - 1 }}</span>
        </span>
        <!-- Linked review session → open it; button shows its state -->
        <Button
          v-if="item.existing_run_id"
          variant="outline"
          :size="compact ? 'xs' : 'sm'"
          :title="runTitle"
          :aria-label="runTitle"
          @click="emit('open-run', item)"
        >
          <RefreshCw v-if="running" class="h-3.5 w-3.5" :stroke-width="2" />
          <Eye v-else class="h-3.5 w-3.5" :stroke-width="2" />
          <span :class="actionLabelClass">{{ runLabel }}</span>
          <span
            v-if="runStatus"
            class="h-1.5 w-1.5 shrink-0 rounded-full"
            :class="[runStatusDotClass(running ? 'running' : runStatus), { 'animate-pulse': running }]"
          />
        </Button>
        <!-- No session yet → start one -->
        <Button
          v-else
          :size="compact ? 'xs' : 'sm'"
          :disabled="reviewing"
          :title="reviewLabel"
          :aria-label="reviewLabel"
          @click="emit('review', item)"
        >
          <Sparkles class="h-3.5 w-3.5" :stroke-width="2" />
          <span :class="actionLabelClass">{{ reviewLabel }}</span>
        </Button>
      </template>
      <!-- Not tracked in devdy: the add action is the whole story -->
      <Button
        v-else
        variant="ghost"
        :size="compact ? 'icon-sm' : 'sm'"
        :title="t('prInbox.addToDevdy')"
        :aria-label="t('prInbox.addToDevdy')"
        @click="emit('add-to-devdy')"
      >
        <FolderPlus class="h-3.5 w-3.5" :stroke-width="2" />
        <span v-if="!compact" class="hidden @[36rem]/list:inline">{{ t('prInbox.addToDevdy') }}</span>
      </Button>
      <Button
        variant="ghost"
        :size="compact ? 'icon-sm' : 'icon'"
        :title="t('prInbox.moreActions')"
        :aria-label="t('prInbox.moreActions')"
        @click="openMenuFromButton"
      >
        <MoreHorizontal class="h-4 w-4" :stroke-width="1.75" />
      </Button>
    </div>
  </Card>
</template>
