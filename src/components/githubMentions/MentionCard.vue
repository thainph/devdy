<script setup lang="ts">
// One GitHub mention (GithubMentionsView), on the shared InboxCard row — the
// same shape as a PR Inbox row: title, `repo#num · who tagged you`, the time
// column, and one line of the AI's summary of what is wanted (the comment
// itself on hover). Why it sits in its tab is the tab's job, not the row's.
// "Check with AI" opens a session on the issue / PR, like the PR Inbox's
// "AI review"; once one exists the button opens it — the row's only button,
// which also carries the unread dot. Every other action (open, copy link,
// + Todo, move, dismiss, mute) is in the right-click menu the view owns.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import InboxCard, { type InboxCardAccount } from '@/components/inbox/InboxCard.vue'
import InboxActionButton from '@/components/inbox/InboxActionButton.vue'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { categoryOf, isStale, mentionedAt, type MentionCheckStatus, type MentionGroupBy, type MentionItem } from '@/lib/githubMentions'
import { Sparkles, User } from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  item: MentionItem
  /** State of the "Check with AI" session for this mention. */
  checkStatus: MentionCheckStatus
  /** A session is being created for it right now. */
  starting: boolean
  /** Status of the linked session (live wins over the fetch snapshot). */
  runStatus?: string | null
  /** Dimension the surrounding group header already shows. */
  groupBy?: MentionGroupBy
  /** Prefix the repo with `owner/` (its bare name is ambiguous in the list). */
  showOwner?: boolean
  /** Several GitHub accounts → show which one was tagged. */
  showAccount?: boolean
  /** `bg-*` dot class for an account id (stable per configured account). */
  accountTone?: (accountId: string) => string
  /** Keyboard selection (j / k). */
  selected?: boolean
  index?: number
}>(), {
  runStatus: null, groupBy: 'none', showOwner: false, showAccount: false,
  accountTone: () => 'bg-muted-foreground', selected: false, index: 0,
})
const emit = defineEmits<{
  select: [item: MentionItem]
  menu: [item: MentionItem, x: number, y: number]
  check: [item: MentionItem]
}>()

const { t } = useI18n()
const store = useGithubMentionsStore()

const stale = computed(() => categoryOf(props.item) === 'reply' && isStale(props.item))
const settled = computed(() => categoryOf(props.item) === 'done')
const unread = computed(() => props.item.unread && !settled.value)
const author = computed(() => props.item.trigger?.author_login ?? '')

/** The tagging comment, flattened — the summary's tooltip, or the line itself without a summary. */
const comment = computed(() => {
  const body = (props.item.trigger?.body ?? '').replace(/\s+/g, ' ').trim()
  return body && author.value ? `${author.value}: ${body}` : body
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

const account = computed<InboxCardAccount | null>(() => {
  if (!props.showAccount) return null
  const label = props.item.account_label || props.item.account_login
  return { label, tone: props.accountTone(props.item.account_id), title: t('githubMentions.taggedAs', { account: label }) }
})

const actionState = computed(() => {
  if (props.checkStatus === 'running' || props.checkStatus === 'done') return props.checkStatus
  return props.starting ? 'starting' : 'none'
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
  <InboxCard
    :title="item.title"
    :kind="item.kind"
    :owner="item.owner"
    :repo="item.repo"
    :number="item.number"
    :show-owner="showOwner"
    :show-repo="groupBy !== 'repo'"
    :time="relativeTime(mentionedAt(item))"
    :stale="stale"
    :stale-title="t('githubMentions.staleTitle')"
    :project-name="groupBy === 'project' ? null : item.project_name"
    :account="account"
    :selected="selected"
    :index="index"
    :muted="settled"
    @select="emit('select', item)"
    @open="open"
    @menu="(x, y) => emit('menu', item, x, y)"
  >
    <template #avatar>
      <span class="block" :title="author || item.title">
        <PrAvatar :url="item.trigger?.author_avatar_url ?? null" :name="author || item.repo" :size="32" />
      </span>
    </template>

    <template #meta>
      <span v-if="author && groupBy !== 'person'" class="hidden @[36rem]/list:flex min-w-0 items-center gap-1">
        <User class="h-3 w-3 shrink-0" :stroke-width="1.75" />
        <span class="truncate">{{ author }}</span>
      </span>
    </template>

    <!-- One line: the AI's summary (the comment on hover), else the comment -->
    <template #body>
      <p
        v-if="item.summary"
        class="flex min-w-0 items-center gap-1.5 text-xs text-foreground/85"
        :title="comment || undefined"
      >
        <Sparkles class="h-3 w-3 shrink-0 text-primary" :stroke-width="1.75" :aria-label="t('githubMentions.summaryLabel')" />
        <span class="min-w-0 truncate">{{ item.summary }}</span>
      </p>
      <p v-else-if="comment" class="truncate text-xs text-foreground/70" :title="comment">{{ comment }}</p>
    </template>

    <!-- Open the linked session, or start one (not on settled mentions) -->
    <template v-if="actionState !== 'none' || !settled" #action>
      <InboxActionButton
        :state="actionState"
        :title="checkTitle"
        :run-status="runStatus"
        :disabled="actionState === 'none' && checkStatus !== 'none'"
        :unread="unread"
        :unread-label="t('githubMentions.unread')"
        @click="emit('check', item)"
      />
    </template>
  </InboxCard>
</template>
