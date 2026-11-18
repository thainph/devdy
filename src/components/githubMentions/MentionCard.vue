<script setup lang="ts">
// One GitHub mention (GithubMentionsView): who asked, where, why it reached
// the user, and the comment waiting for an answer. "Reply" opens a composer
// under the excerpt; "AI draft" opens it pre-filled. The reply is posted as
// the account the mention reached (in the review thread for a diff comment).
// Right-click opens the card menu (the view owns it).
import { computed, nextTick, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Button, Card, Textarea } from '@/components/ui'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import AddTodoButton from '@/components/focus/AddTodoButton.vue'
import { useToast } from '@/composables/useToast'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { isStale, mentionedAt, type MentionItem } from '@/lib/githubMentions'
import type { TodoPrefill } from '@/lib/focus'
import {
  AtSign, Check, CircleDot, ExternalLink, GitPullRequest, Loader2, MessageSquare, Reply, Sparkles, Users,
} from 'lucide-vue-next'

const props = defineProps<{ item: MentionItem }>()
const emit = defineEmits<{ menu: [item: MentionItem, x: number, y: number] }>()

const { t } = useI18n()
const { toast } = useToast()
const store = useGithubMentionsStore()

const composing = ref(false)
const text = ref('')
const drafting = ref(false)
const drafted = ref(false)
const textarea = ref<InstanceType<typeof Textarea> | null>(null)

const busy = computed(() => store.busyKeys.has(props.item.key))
const canReply = computed(() => props.item.number !== null && !props.item.answered)
const stale = computed(() => isStale(props.item))
const ref_ = computed(() => {
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
  text: t('githubMentions.todoText', { title: props.item.title, ref: ref_.value }),
  sourceUrl: props.item.trigger?.html_url || props.item.html_url,
}))

/** Body excerpt split so `@handles` can be highlighted. */
const excerptParts = computed(() => {
  const body = (props.item.trigger?.body ?? '').replace(/\s+/g, ' ').trim()
  return body.split(/(@[\w-]+(?:\/[\w-]+)?)/g).filter(Boolean).map(s => ({ text: s, at: s.startsWith('@') }))
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

async function startReply() {
  composing.value = true
  await nextTick()
  ;(textarea.value?.$el as HTMLTextAreaElement | undefined)?.focus()
}

// Bumped by Cancel, so a draft that lands after it is dropped.
let draftRun = 0

async function draft() {
  const run = ++draftRun
  composing.value = true
  drafting.value = true
  try {
    const result = await store.draft(props.item.key)
    if (run !== draftRun) return
    text.value = result
    drafted.value = true
  } catch (e) {
    if (run === draftRun) toast.error(t('githubMentions.draftFailed', { error: String(e) }))
  } finally {
    if (run === draftRun) drafting.value = false
  }
}

function cancel() {
  draftRun++
  drafting.value = false
  composing.value = false
  text.value = ''
  drafted.value = false
}

async function send() {
  if (!text.value.trim() || busy.value) return
  try {
    await store.reply(props.item.key, text.value)
    toast.success(t('githubMentions.sent'))
    cancel()
  } catch (e) {
    toast.error(t('githubMentions.sendFailed', { error: String(e) }))
  }
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    void send()
  } else if (e.key === 'Escape' && !text.value.trim()) {
    cancel()
  }
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
        <span class="font-mono text-[10.5px]">{{ ref_ }}</span>
        <span v-if="item.answered" class="inline-flex items-center gap-1 rounded-[5px] bg-success/10 px-1.5 py-0.5 text-[10.5px] font-medium text-success">
          <Check class="h-3 w-3" :stroke-width="2" />
          {{ t('githubMentions.answered', { time: relativeTime(item.answered_at ?? item.updated_at) }) }}
        </span>
        <span v-else class="inline-flex items-center gap-1 rounded-[5px] px-1.5 py-0.5 text-[10.5px] font-medium" :class="reasonChip.cls">
          <component :is="reasonChip.icon" class="h-3 w-3" :stroke-width="2" />
          {{ reasonLabel }}
        </span>
        <span class="rounded-[5px] bg-muted px-1.5 py-0.5 text-[10.5px] font-medium">{{ item.account_label || item.account_login }}</span>
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

      <!-- Composer -->
      <div v-if="composing" class="mt-1 overflow-hidden rounded-lg border border-border bg-background">
        <div class="flex items-center gap-1.5 border-b border-border/60 px-2 py-1.5 text-[11px] text-muted-foreground">
          <template v-if="drafting">
            <Loader2 class="h-3 w-3 animate-spin text-primary" :stroke-width="2" />
            {{ t('githubMentions.drafting') }}
          </template>
          <template v-else-if="drafted">
            <Sparkles class="h-3 w-3 text-primary" :stroke-width="2" />
            <span class="font-medium text-primary">{{ t('githubMentions.draftReady') }}</span>
            · {{ t('githubMentions.draftHint') }}
          </template>
          <template v-else>
            <Reply class="h-3 w-3" :stroke-width="2" />
            {{ t('githubMentions.replyTo', { who: author || item.repo }) }}
          </template>
        </div>
        <Textarea
          ref="textarea"
          v-model="text"
          class="min-h-24 rounded-none border-0 focus:ring-0"
          :disabled="drafting || busy"
          :placeholder="t('githubMentions.replyPlaceholder')"
          :aria-label="t('githubMentions.replyPlaceholder')"
          @keydown="onKey"
        />
        <div class="flex items-center gap-2 border-t border-border/60 px-2 py-1.5">
          <span class="mr-auto truncate text-[11px] text-muted-foreground">
            {{ t('githubMentions.sendAs', { account: item.account_login }) }}
          </span>
          <Button variant="ghost" size="xs" :disabled="drafting || busy" @click="draft">
            <Sparkles class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ drafted ? t('githubMentions.redraft') : t('githubMentions.aiDraft') }}
          </Button>
          <Button variant="ghost" size="xs" :disabled="busy" @click="cancel">{{ t('common.cancel') }}</Button>
          <Button size="xs" :disabled="!text.trim() || drafting || busy" @click="send">
            <Loader2 v-if="busy" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
            <Reply v-else class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('githubMentions.send') }}
          </Button>
        </div>
      </div>
    </div>

    <div class="flex shrink-0 items-start gap-1">
      <span
        class="whitespace-nowrap pt-1.5 pr-1 text-[11px] tabular-nums"
        :class="stale ? 'text-warning' : 'text-muted-foreground'"
        :title="stale ? t('githubMentions.staleTitle') : undefined"
      >
        {{ relativeTime(mentionedAt(item)) }}
      </span>
      <template v-if="!composing">
        <Button v-if="canReply" size="sm" :title="t('githubMentions.reply')" @click="startReply">
          <Reply class="h-3.5 w-3.5" :stroke-width="2" />
          <span class="hidden xl:inline">{{ t('githubMentions.reply') }}</span>
        </Button>
        <Button
          v-if="canReply"
          variant="ghost"
          size="icon"
          :title="t('githubMentions.aiDraft')"
          :aria-label="t('githubMentions.aiDraft')"
          @click="draft"
        >
          <Sparkles class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </template>
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
