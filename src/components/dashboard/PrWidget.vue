<script setup lang="ts">
// Work Dashboard — GitHub PRs where the user was asked to review directly:
// how many wait on them, then "Review again" (new commits after the AI
// review) → "To review" (quick reviews first, drafts last) → "Bots". Each row
// leads with a status icon and says it in words: #n title, repo · size S/M/L,
// CI as icon + words (passing / failing / running), and acts: Review again /
// AI review (Sparkles) / Open session, Open the PR, todo "Review PR #n"; failing
// CI says "waiting for the author to fix CI"; an unmapped repo offers "Map
// repo". No time axis: the wait is not the main signal here.
import { computed, type Component } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  Bot, CircleCheck, CircleX, ExternalLink, FolderPlus, GitPullRequest, ListPlus, LoaderCircle, RefreshCw, Sparkles,
} from 'lucide-vue-next'
import { Badge, Banner, Button } from '@/components/ui'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import { fitGroups, prCardGroups, prLevel, prSize } from '@/lib/dashboardLayout'
import type { PrPriorityResult, RankedPr } from '@/lib/prPriority'
import type { PrInboxItem } from '@/stores/prInbox'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import DashGroup from './DashGroup.vue'
import DashRow from './DashRow.vue'
import { CATEGORY_ICON, LEVEL_TEXT, type Tone } from './levels'

const props = defineProps<{
  ranked: PrPriorityResult<PrInboxItem>
  actionable: number
  state: SourceState
  now: number
  /** PRs whose review is being started. */
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

const groups = computed(() => fitGroups(prCardGroups(props.ranked), WIDGET_ROWS))
const level = computed(() => prLevel(props.ranked))

const SIZE_LETTER = { small: 'S', medium: 'M', large: 'L' } as const
const CI = {
  success: { icon: CircleCheck, cls: 'text-success' },
  failure: { icon: CircleX, cls: 'text-destructive' },
  pending: { icon: LoaderCircle, cls: 'text-warning' },
} as const

function rowIcon(key: string, r: RankedPr<PrInboxItem>): { icon: Component, tone: Tone } {
  if (key === 'reReview') return { icon: RefreshCw, tone: 'warn' }
  if (key === 'bots') return { icon: Bot, tone: 'none' }
  return { icon: GitPullRequest, tone: r.ci === 'failure' ? 'bad' : r.pr.draft ? 'none' : 'primary' }
}

/** The words after the size chip (the full line is the tooltip, `sub`). */
function details(r: RankedPr<PrInboxItem>): string {
  const parts: string[] = []
  if (r.ai !== 'none') parts.push(t(`dashboard.prs.ai.${r.ai}`))
  if (r.soleReviewer) parts.push(t('dashboard.prs.soleReviewer'))
  if (r.pr.author_login) parts.push(r.pr.author_login)
  return parts.join(' · ')
}

function sub(r: RankedPr<PrInboxItem>): string {
  const parts = [r.pr.repo]
  const size = prSize(r.pr)
  if (size) parts.push(t(`dashboard.prs.size.${size}`))
  if (r.ci) parts.push(t(`dashboard.prs.ci.${r.ci}`))
  if (r.ci === 'failure') parts.push(t('dashboard.prs.ciWaitAuthor'))
  if (r.ai !== 'none') parts.push(t(`dashboard.prs.ai.${r.ai}`))
  if (r.soleReviewer) parts.push(t('dashboard.prs.soleReviewer'))
  if (r.pr.author_login) parts.push(r.pr.author_login)
  return parts.join(' · ')
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.prs.title')"
    :icon="CATEGORY_ICON.prs"
    :level="level"
    :scope="t('dashboard.prs.scope')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :see-all="t('dashboard.prs.seeAll')"
    @see-all="emit('openAll')"
  >
    <template #answer>
      <span class="text-3xl font-semibold leading-none tabular-nums" :class="LEVEL_TEXT[actionable ? level : 'none']">{{ actionable }}</span>
      <span class="text-[13px] font-medium">{{ t('dashboard.prs.answer', actionable) }}</span>
    </template>

    <Banner v-if="state.error" tone="warning" class="mx-4 my-2">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <p v-if="!groups.length" class="border-t border-border/50 px-4 py-5 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.prs.empty') }}
    </p>
    <div v-else class="border-t border-border/50">
      <DashGroup
        v-for="g in groups"
        :key="g.key"
        :label="t(`dashboard.prs.groups.${g.key}`)"
        :count="g.total"
        :hidden="g.hidden"
        @more="emit('openAll')"
      >
        <DashRow
          v-for="r in g.shown"
          :key="`${r.pr.owner}/${r.pr.repo}#${r.pr.number}`"
          :title="`#${r.pr.number} ${r.pr.title}`"
          :sub="sub(r)"
          :hint="t('dashboard.prs.openPr')"
          :icon="rowIcon(g.key, r).icon"
          :tone="rowIcon(g.key, r).tone"
          @open="emit('openPr', r.pr)"
        >
          <template #line>
            <span class="max-w-[50%] shrink-0 truncate">{{ r.pr.repo }}</span>
            <span
              v-if="prSize(r.pr)"
              class="shrink-0 rounded bg-muted px-1 text-[10px] font-semibold leading-4 text-muted-foreground"
              :title="t(`dashboard.prs.size.${prSize(r.pr)}`)"
            >{{ SIZE_LETTER[prSize(r.pr)!] }}<span class="sr-only"> {{ t(`dashboard.prs.size.${prSize(r.pr)}`) }}</span></span>
            <span v-if="r.ci === 'failure'" class="shrink-0 font-medium text-destructive">· {{ t('dashboard.prs.ciWaitAuthor') }}</span>
            <span v-if="details(r)" class="min-w-0 truncate">· {{ details(r) }}</span>
          </template>
          <template #aside>
            <Badge v-if="r.pr.draft" tone="neutral" size="xs">{{ t('dashboard.prs.draft') }}</Badge>
            <span
              v-if="r.ci"
              class="inline-flex shrink-0 items-center gap-0.5 text-[11px] font-medium"
              :class="CI[r.ci!].cls"
              :title="t(`dashboard.prs.ci.${r.ci}`)"
            >
              <component
                :is="CI[r.ci!].icon"
                class="h-3.5 w-3.5"
                :class="r.ci === 'pending' && 'animate-spin motion-reduce:animate-none'"
                :stroke-width="2"
                aria-hidden="true"
              />
              <span class="sr-only @xl/card:not-sr-only">{{ t(`dashboard.prs.ci.${r.ci}`) }}</span>
            </span>
            <template v-if="g.key !== 'bots'">
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
                variant="primary"
                size="xs"
                :disabled="busy(r.pr)"
                @click="emit('reReview', r.pr)"
              >
                <Sparkles class="h-3 w-3" :stroke-width="1.75" />{{ t('dashboard.prs.reReview') }}
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
                :title="t('dashboard.prs.addTodo', { n: r.pr.number })"
                :aria-label="t('dashboard.prs.addTodo', { n: r.pr.number })"
                @click="emit('addTodo', r.pr)"
              >
                <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </template>
            <Button
              variant="ghost"
              size="icon-sm"
              :title="t('dashboard.prs.openPr')"
              :aria-label="t('dashboard.prs.openPr')"
              @click="emit('openPr', r.pr)"
            >
              <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
        </DashRow>
      </DashGroup>
    </div>
  </DashboardWidget>
</template>
