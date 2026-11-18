<script setup lang="ts">
// One GitHub mention (GithubMentionsView): who asked, where, why it reached
// the user, and the comment waiting for an answer. "Check with AI" opens a
// session on the issue / PR, like the PR Inbox's "AI review"; once one exists
// the button opens it. Right-click opens the card menu (the view owns both).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Button, Card } from '@/components/ui'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import AddTodoButton from '@/components/focus/AddTodoButton.vue'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { isStale, mentionedAt, type MentionCheckStatus, type MentionItem } from '@/lib/githubMentions'
import type { TodoPrefill } from '@/lib/focus'
import {
  AtSign, Check, CircleDot, ExternalLink, Eye, GitPullRequest, Loader2, MessageSquare, Reply, Sparkles, Users,
} from 'lucide-vue-next'

const props = defineProps<{
  item: MentionItem
  /** State of the "Check with AI" session for this mention. */
  checkStatus: MentionCheckStatus
  /** A session is being created for it right now. */
  starting: boolean
}>()
const emit = defineEmits<{
  menu: [item: MentionItem, x: number, y: number]
  check: [item: MentionItem]
}>()

const { t } = useI18n()
const store = useGithubMentionsStore()

const stale = computed(() => isStale(props.item))
const refText = computed(() => {
  const repo = `${props.item.owner}/${props.item.repo}`
  if (props.item.kind === 'Discussion') return `${repo} · Discussion`
  return props.item.number !== null ? `${repo}#${props.item.number}` : repo
})
const kindIcon = computed(() =>
  props.item.kind === 'PullRequest' ? GitPullRequest : props.item.kind === 'Issue' ? CircleDot : MessageSquare,
)
const kindClass = computed(() =>
  props.item.kind === 'PullRequest' ? 'text-success' : props.item.kind === 'Issue' ? 'text-info' : 'text-warning',
)
const reasonChip = computed(() => {
  switch (props.item.reason) {
    case 'mention': return { icon: AtSign, cls: 'bg-primary/10 text-primary' }
    case 'team_mention': return { icon: Users, cls: 'bg-info/10 text-info' }
    case 'comment': return { icon: Reply, cls: 'bg-warning/10 text-warning' }
    default: return { icon: MessageSquare, cls: 'bg-success/10 text-success' }
  }
})
const reasonLabel = computed(() => {
  const r = props.item.reason
  return ['mention', 'team_mention', 'comment', 'author'].includes(r) ? t(`githubMentions.reason.${r}`) : r
})
const author = computed(() => props.item.trigger?.author_login ?? '')

const todoPrefill = computed<TodoPrefill>(() => ({
  text: t('githubMentions.todoText', { title: props.item.title, ref: refText.value }),
  projectId: props.item.project_id ?? undefined,
  sourceUrl: props.item.trigger?.html_url || props.item.html_url,
}))

/** Body excerpt split so `@handles` can be highlighted. */
const excerptParts = computed(() => {
  const body = (props.item.trigger?.body ?? '').replace(/\s+/g, ' ').trim()
  return body.split(/(@[\w-]+(?:\/[\w-]+)?)/g).filter(Boolean).map(s => ({ text: s, at: s.startsWith('@') }))
})

const checkTitle = computed(() => {
  switch (props.checkStatus) {
    case 'unavailable': return t('githubMentions.check.unavailable')
    case 'unmapped': return t('githubMentions.check.unmapped', { repo: `${props.item.owner}/${props.item.repo}` })
    case 'running': return t('githubMentions.check.running')
    case 'done': return t('githubMentions.check.open')
    default: return t('githubMentions.check.start')
  }
})

function relativeTime(iso: string): string {
  const diff = Date.now() - Date.parse(iso)
  if (!Number.isFinite(diff)) return ''
  const min = Math.max(0, Math.round(diff / 60_000))
  if (min < 60) return t('githubMentions.time.minutes', { n: Math.max(1, min) })
  const h = Math.round(min / 60)
  if (h < 24) return t('githubMentions.time.hours', { n: h })
  return t('githubMentions.time.days', { n: Math.round(h / 24) })
}

function open() {
  const url = props.item.trigger?.html_url || props.item.html_url
  openUrl(url).catch(() => { /* opener unavailable */ })
  if (props.item.unread) store.markRead(props.item.key).catch(() => { /* stays unread */ })
}
</script>

<template>
  <Card
    class="transition-colors hover:border-primary/40"
    body-class="flex gap-3 px-3.5 py-3"
    :class="[item.unread && !item.answered ? 'shadow-[inset_3px_0_0_0_hsl(var(--primary))]' : '', item.answered ? 'opacity-70' : '']"
    @contextmenu.prevent="emit('menu', item, $event.clientX, $event.clientY)"
  >
    <span :title="author || item.title">
      <PrAvatar :url="item.trigger?.author_avatar_url ?? null" :name="author || item.repo" :size="32" />
    </span>

    <div class="flex min-w-0 flex-1 flex-col gap-1.5">
      <div class="flex min-w-0 items-center gap-1.5">
        <component :is="kindIcon" class="h-3.5 w-3.5 shrink-0" :class="kindClass" :stroke-width="2" />
        <button
          type="button"
          class="min-w-0 truncate text-left text-sm font-semibold leading-tight transition-colors hover:text-primary cursor-pointer"
          :title="item.title"
          @click="open"
        >
          {{ item.title }}
        </button>
        <span v-if="item.unread && !item.answered" class="sr-only">({{ t('githubMentions.unread') }})</span>
      </div>

      <div class="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1 text-[11px] text-muted-foreground">
        <span class="font-mono text-[10.5px]">{{ refText }}</span>
        <span v-if="item.answered" class="inline-flex items-center gap-1 rounded-[5px] bg-success/10 px-1.5 py-0.5 text-[10.5px] font-medium text-success">
          <Check class="h-3 w-3" :stroke-width="2" />
          {{ t('githubMentions.answered', { time: relativeTime(item.answered_at ?? item.updated_at) }) }}
        </span>
        <span v-else class="inline-flex items-center gap-1 rounded-[5px] px-1.5 py-0.5 text-[10.5px] font-medium" :class="reasonChip.cls">
          <component :is="reasonChip.icon" class="h-3 w-3" :stroke-width="2" />
          {{ reasonLabel }}
        </span>
        <span class="rounded-[5px] bg-muted px-1.5 py-0.5 text-[10.5px] font-medium">{{ item.account_label || item.account_login }}</span>
        <span v-if="item.project_name" class="truncate">{{ item.project_name }}</span>
        <span v-if="item.trigger?.path" class="min-w-0 truncate">
          {{ t('githubMentions.onFile') }} <span class="font-mono text-[10.5px]">{{ item.trigger.path }}</span>
        </span>
      </div>

      <p
        v-if="item.trigger && excerptParts.length"
        class="line-clamp-2 border-l-2 border-border pl-2.5 text-[12.5px] leading-relaxed text-foreground/85"
      >
        <span class="font-semibold text-foreground">{{ author }}:</span>
        {{ ' ' }}<template v-for="(p, i) in excerptParts" :key="i"><span v-if="p.at" class="rounded-sm bg-primary/10 px-0.5 font-semibold text-primary">{{ p.text }}</span><template v-else>{{ p.text }}</template></template>
      </p>
      <span v-if="item.newer_comments > 0 && !item.answered" class="text-[11px] text-muted-foreground">
        {{ t('githubMentions.newerComments', item.newer_comments) }}
      </span>
    </div>

    <div class="flex shrink-0 items-start gap-1">
      <span
        class="whitespace-nowrap pt-1.5 pr-1 text-[11px] tabular-nums"
        :class="stale ? 'text-warning' : 'text-muted-foreground'"
        :title="stale ? t('githubMentions.staleTitle') : undefined"
      >
        {{ relativeTime(mentionedAt(item)) }}
      </span>
      <Button
        v-if="checkStatus === 'running' || checkStatus === 'done'"
        variant="outline"
        size="sm"
        :title="checkTitle"
        @click="emit('check', item)"
      >
        <Loader2 v-if="checkStatus === 'running'" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
        <Eye v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        <span class="hidden xl:inline">{{ checkStatus === 'running' ? t('githubMentions.check.runningLabel') : t('githubMentions.check.openLabel') }}</span>
      </Button>
      <Button
        v-else-if="!item.answered"
        size="sm"
        :disabled="starting || checkStatus !== 'none'"
        :title="checkTitle"
        :aria-label="t('githubMentions.check.label')"
        @click="emit('check', item)"
      >
        <Loader2 v-if="starting" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
        <Sparkles v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        <span class="hidden xl:inline">{{ t('githubMentions.check.label') }}</span>
      </Button>
      <Button
        variant="ghost"
        size="icon"
        :title="t('githubMentions.openOnGithub')"
        :aria-label="t('githubMentions.openOnGithub')"
        @click="open"
      >
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <AddTodoButton v-if="!item.answered" :prefill="todoPrefill" variant="icon" button-variant="ghost" size="icon" />
    </div>
  </Card>
</template>
