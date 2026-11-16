<script setup lang="ts">
// Work Dashboard — GitHub PRs where the user was asked to review directly,
// ranked by `prPriority` (re-review after new commits, long waits, green CI,
// small PRs, last reviewer; drafts last, bots folded into one row). Each row
// shows the wait (from the review request; "updated" otherwise), CI, size and
// the AI review badge. Actions: run / open / redo the AI review, open the PR,
// make a todo "Review PR #n"; an unmapped repo offers "Map repo to a project".
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Bot, ExternalLink, FolderPlus, GitPullRequest, ListPlus, RotateCcw, Sparkles } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import type { PrPriorityResult, RankedPr } from '@/lib/prPriority'
import type { PrInboxItem } from '@/stores/prInbox'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import { useDurationLabel } from './useDurationLabel'

const props = defineProps<{
  ranked: PrPriorityResult<PrInboxItem>
  actionable: number
  state: SourceState
  now: number
  /** Keys (owner/repo#n) of PRs whose review is being started. */
  busy: (item: PrInboxItem) => boolean
}>()

const emit = defineEmits<{
  review: [item: PrInboxItem]
  reReview: [item: PrInboxItem]
  openPr: [item: PrInboxItem]
  addTodo: [item: PrInboxItem]
  mapRepo: []
  openAll: []
}>()

const { t } = useI18n()
const { duration } = useDurationLabel()

const DAY = 86_400_000
const shown = computed(() => props.ranked.rows.slice(0, WIDGET_ROWS))
const total = computed(() => props.ranked.rows.length + props.ranked.bots.length)

const WAIT_CLASS = { ok: 'text-muted-foreground', warn: 'text-warning', late: 'text-destructive' } as const

function timeLabel(r: RankedPr<PrInboxItem>): string {
  if (r.waitDays !== null) return t('dashboard.prs.waiting', { duration: duration(r.waitDays * DAY) })
  const at = Date.parse(r.pr.updated_at)
  return Number.isNaN(at) ? '' : t('dashboard.prs.updated', { duration: duration(Math.max(0, props.now - at)) })
}

function sizeLabel(r: RankedPr<PrInboxItem>): string | null {
  const a = r.pr.additions
  const d = r.pr.deletions
  if (a == null && d == null) return null
  return `+${a ?? 0} −${d ?? 0}`
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.prs.title')"
    :icon="GitPullRequest"
    :count="actionable"
    :count-title="t('dashboard.prs.countTitle')"
    :scope="t('dashboard.prs.scope')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :see-all="t('dashboard.seeAll', { n: total })"
    @see-all="emit('openAll')"
  >
    <Banner v-if="state.error" tone="warning" class="m-3">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <p v-if="shown.length === 0 && ranked.bots.length === 0" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.prs.empty') }}
    </p>
    <ul v-else class="divide-y divide-border/40">
      <li v-for="r in shown" :key="`${r.pr.owner}/${r.pr.repo}#${r.pr.number}`" class="group flex items-start gap-2 px-3 py-2 hover:bg-accent/50">
        <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" @click="emit('openPr', r.pr)">
          <p class="truncate text-[13px] text-foreground">
            <span class="text-muted-foreground">{{ r.pr.repo }}#{{ r.pr.number }}</span> {{ r.pr.title }}
          </p>
          <div class="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
            <span :class="WAIT_CLASS[r.waitLevel]" :title="r.waitDays === null ? t('dashboard.prs.noRequestTime') : undefined">{{ timeLabel(r) }}</span>
            <span>· {{ r.pr.author_login }}</span>
            <span v-if="r.pr.draft">· {{ t('dashboard.prs.draft') }}</span>
            <span v-if="r.ci === 'failure'" class="font-medium text-destructive">· {{ t('dashboard.prs.ciFailed') }}</span>
            <span v-else-if="r.ci === 'success'" class="text-success">· {{ t('dashboard.prs.ciPassed') }}</span>
            <span v-else-if="r.ci === 'pending'">· {{ t('dashboard.prs.ciPending') }}</span>
            <span v-if="sizeLabel(r)" :class="r.small && 'text-success'">· {{ sizeLabel(r) }}</span>
            <span v-if="r.soleReviewer">· {{ t('dashboard.prs.soleReviewer') }}</span>
            <span
              class="font-medium"
              :class="r.ai === 'updated' ? 'text-warning' : r.ai === 'running' ? 'text-info' : r.ai === 'done' ? 'text-success' : ''"
            >· {{ t(`dashboard.prs.ai.${r.ai}`) }}</span>
          </div>
        </button>
        <div class="flex shrink-0 items-center gap-0.5">
          <Button
            v-if="!r.pr.mapped"
            variant="ghost"
            size="xs"
            :title="t('dashboard.prs.mapRepoHint')"
            @click="emit('mapRepo')"
          >
            <FolderPlus class="h-3 w-3" :stroke-width="1.75" />{{ t('dashboard.prs.mapRepo') }}
          </Button>
          <Button
            v-else-if="r.ai === 'updated'"
            variant="outline"
            size="xs"
            :disabled="busy(r.pr)"
            @click="emit('reReview', r.pr)"
          >
            <RotateCcw class="h-3 w-3" :stroke-width="1.75" />{{ t('dashboard.prs.reReview') }}
          </Button>
          <Button
            v-else
            :variant="r.ai === 'none' ? 'outline' : 'ghost'"
            size="xs"
            :disabled="busy(r.pr)"
            @click="emit('review', r.pr)"
          >
            <Sparkles class="h-3 w-3" :stroke-width="1.75" />{{ r.ai === 'none' ? t('dashboard.prs.review') : t('dashboard.prs.openSession') }}
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            class="opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
            :title="t('dashboard.prs.addTodo', { n: r.pr.number })"
            :aria-label="t('dashboard.prs.addTodo', { n: r.pr.number })"
            @click="emit('addTodo', r.pr)"
          >
            <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            class="opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
            :title="t('dashboard.prs.openPr')"
            :aria-label="t('dashboard.prs.openPr')"
            @click="emit('openPr', r.pr)"
          >
            <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
        </div>
      </li>
      <li v-if="ranked.bots.length" class="flex items-center gap-2 px-3 py-2">
        <button type="button" class="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 text-left text-[11px] text-muted-foreground hover:text-foreground" @click="emit('openAll')">
          <Bot class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          {{ t('dashboard.prs.bots', { n: ranked.bots.length }, ranked.bots.length) }}
        </button>
      </li>
    </ul>
  </DashboardWidget>
</template>
