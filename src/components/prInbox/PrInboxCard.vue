<script setup lang="ts">
// One PR in the inbox (PrInboxView). `comfortable` is the full two-line card;
// `compact` squeezes it into a single row: title · repo#num · author · time ·
// badges · action. Review / navigation stay in the view — this only renders and
// emits, so both densities share one action path.
//
// Responsive: sized by the `@container/list` around the PR list (PrInboxView),
// so it reacts to the content pane, not the window. As the list narrows,
// lower-priority bits drop out (36rem / 44rem / 52rem) and action buttons go
// icon-only (title + aria-label kept), so the title always keeps room.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import type { PrInboxItem } from '@/stores/prInbox'
import { isStale, reviewerAccountsOf, splitLabels, STALE_AFTER_DAYS } from '@/lib/prInboxFilters'
import { Button, Card, Badge, StatusBadge } from '@/components/ui'
import {
  GitPullRequest, RefreshCw, Sparkles, FolderPlus, ExternalLink, User, Users, Eye, Bot, MoreHorizontal,
} from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  item: PrInboxItem
  density: 'comfortable' | 'compact'
  /** Inside a group header (slightly tighter padding than the flat list). */
  grouped?: boolean
  /** Backend bot flag OR the user's custom bot list. */
  isBot: boolean
  /** Effective status of the linked review run (live wins over DB snapshot). */
  runStatus: string | null
  /** A review run is being created for this PR right now. */
  reviewing: boolean
  /** Epoch ms used for the stale badge (passed in so all cards agree). */
  now: number
  index?: number
}>(), { grouped: false, index: 0 })

const emit = defineEmits<{
  review: [item: PrInboxItem]
  'open-run': [item: PrInboxItem]
  /** Open the card menu at a viewport point (⋯ button or right-click). */
  menu: [item: PrInboxItem, x: number, y: number]
}>()

const { t } = useI18n()
const router = useRouter()

const compact = computed(() => props.density === 'compact')
const running = computed(() => props.runStatus === 'running' || props.runStatus === 'fetched')
const runLabel = computed(() => (running.value ? t('prInbox.reviewingShort') : t('prInbox.viewReview')))
const reviewLabel = computed(() => (props.reviewing ? t('prInbox.opening') : t('prInbox.aiReview')))
// Action labels show from this container width up; below it, icon-only.
const actionLabelClass = computed(() => (compact.value ? 'hidden @[44rem]/list:inline' : 'hidden @[36rem]/list:inline'))
const stale = computed(() => isStale(props.item.updated_at, props.now))
const labels = computed(() => splitLabels(props.item.labels ?? []))
const accounts = computed(() => reviewerAccountsOf(props.item))
const reviewers = computed(() => props.item.requested_reviewers ?? [])
const shownReviewers = computed(() => reviewers.value.slice(0, 4))
const reviewersTitle = computed(() =>
  // Teams arrive as `org/slug` (GitHub's combinedSlug) — same `@` form as GitHub.
  reviewers.value.map(r => `@${r.login_or_slug}`).join(', '),
)

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
    :body-class="`flex flex-row items-center flex-1 min-w-0 ${compact ? 'gap-3 px-3 py-1.5' : grouped ? 'gap-4 p-3' : 'gap-4 p-4'}`"
    @contextmenu.prevent="emit('menu', item, $event.clientX, $event.clientY)"
  >
    <!-- Icon (comfortable only — compact keeps the row to one line) -->
    <div
      v-if="!compact"
      class="hidden @[36rem]/list:flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary"
    >
      <GitPullRequest class="h-4.5 w-4.5" :stroke-width="1.75" />
    </div>

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
        <Badge v-if="isBot" tone="info" size="xs" class="shrink-0">
          <Bot class="h-2.5 w-2.5" :stroke-width="2" />
          {{ t('prInbox.bot') }}
        </Badge>
        <Badge
          v-if="stale"
          tone="warning"
          size="xs"
          class="shrink-0"
          :class="compact ? 'hidden @[44rem]/list:inline-flex' : ''"
          :title="t('prInbox.staleTitle', { days: STALE_AFTER_DAYS })">
          {{ t('prInbox.stale') }}
        </Badge>
        <!-- Labels: wide containers only (contents = no wrapper box) -->
        <span v-if="!compact" class="hidden @[44rem]/list:contents">
          <span
            v-for="label in labels.shown"
            :key="label"
            class="shrink-0 max-w-28 truncate rounded border border-border/70 px-1 py-px text-[9px] text-muted-foreground"
            :title="label"
          >
            {{ label }}
          </span>
          <span
            v-if="labels.extra > 0"
            class="shrink-0 text-[9px] text-muted-foreground"
            :title="item.labels.slice(3).join(', ')"
          >
            +{{ labels.extra }}
          </span>
        </span>
      </div>
      <div
        class="flex min-w-0 items-center gap-2 text-[11px] text-muted-foreground"
        :class="compact ? 'max-w-[45%]' : 'flex-wrap mt-1'"
      >
        <span class="min-w-0 font-mono truncate">{{ item.owner }}/{{ item.repo }} <span class="text-muted-foreground/70">#{{ item.number }}</span></span>
        <span class="min-w-0 items-center gap-1" :class="compact ? 'hidden @[36rem]/list:flex' : 'flex'">
          <User class="h-3 w-3 shrink-0" :stroke-width="1.75" />
          <span class="truncate">{{ item.author_login }}</span>
        </span>
        <span class="shrink-0 text-muted-foreground/60" :class="{ 'hidden @[44rem]/list:inline': compact }">
          {{ relativeTime(item.updated_at) }}
        </span>
        <template v-if="!compact">
          <!-- Project moves here from the right side on narrow containers -->
          <Badge
            v-if="item.mapped"
            tone="primary"
            size="xs"
            class="shrink-0 max-w-40 truncate @[44rem]/list:hidden"
            :title="item.project_name ?? ''"
          >
            {{ item.project_name }}
          </Badge>
          <Badge v-for="acc in accounts" :key="acc.account_id" tone="neutral" size="xs" class="shrink-0">
            {{ acc.account_label }}
          </Badge>
          <!-- Other pending reviewers: avatars for users, chips for teams. -->
          <span v-if="reviewers.length > 0" class="flex items-center gap-1" :title="reviewersTitle">
            <span class="text-muted-foreground/60">{{ t('prInbox.alsoRequested') }}</span>
            <template v-for="r in shownReviewers" :key="`${r.kind}:${r.login_or_slug}`">
              <img
                v-if="r.kind === 'user' && r.avatar_url"
                :src="r.avatar_url"
                :alt="r.login_or_slug"
                class="h-4 w-4 rounded-full bg-muted"
              />
              <span v-else class="flex items-center gap-0.5 rounded border border-border/70 px-1 py-px text-[9px]">
                <Users v-if="r.kind === 'team'" class="h-2.5 w-2.5" :stroke-width="2" />
                {{ r.login_or_slug }}
              </span>
            </template>
            <span v-if="reviewers.length > shownReviewers.length" class="text-[9px]">
              +{{ reviewers.length - shownReviewers.length }}
            </span>
          </span>
        </template>
        <span v-else-if="reviewers.length > 0" class="hidden @[52rem]/list:flex shrink-0 items-center gap-0.5" :title="reviewersTitle">
          <Users class="h-3 w-3" :stroke-width="1.75" />
          {{ reviewers.length }}
        </span>
      </div>
    </div>

    <!-- Mapping + action -->
    <div class="flex items-center gap-2 shrink-0">
      <template v-if="item.mapped">
        <Badge
          v-if="!compact"
          tone="primary"
          size="xs"
          class="hidden @[44rem]/list:inline-flex shrink-0 max-w-40 truncate"
          :title="item.project_name ?? ''"
        >
          {{ item.project_name }}
        </Badge>
        <!-- Linked review session already exists → show its status + open it -->
        <template v-if="item.existing_run_id">
          <StatusBadge
            v-if="runStatus"
            :status="runStatus"
            size="xs"
            :class="compact ? 'hidden @[44rem]/list:inline-flex' : 'hidden @[36rem]/list:inline-flex'"
          />
          <Button
            variant="outline"
            :size="compact ? 'xs' : 'sm'"
            :title="runLabel"
            :aria-label="runLabel"
            @click="emit('open-run', item)"
          >
            <RefreshCw v-if="running" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
            <Eye v-else class="h-3.5 w-3.5" :stroke-width="2" />
            <span :class="actionLabelClass">{{ runLabel }}</span>
          </Button>
        </template>
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
      <template v-else>
        <Badge tone="neutral" size="xs" class="shrink-0" :class="{ 'hidden @[36rem]/list:inline-flex': compact }">
          {{ t('prInbox.notInDevdy') }}
        </Badge>
        <Button
          v-if="!compact"
          variant="ghost"
          :title="t('prInbox.addToDevdy')"
          :aria-label="t('prInbox.addToDevdy')"
          @click="router.push('/projects')"
        >
          <FolderPlus class="h-3.5 w-3.5" :stroke-width="2" />
          <span class="hidden @[36rem]/list:inline">{{ t('prInbox.addToDevdy') }}</span>
        </Button>
      </template>
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
