<script setup lang="ts">
// Shared row of the inbox lists (PR Inbox, GitHub Mentions), so both screens
// read the same: avatar · kind icon + title · meta (repo#num · extras) ·
// optional one-line body · account · time column · primary action. Rows sit
// in an InboxList frame (divided rows, no per-item card). `#meta` / `#body`
// add what only one list has; the primary action goes in `#action`
// (InboxActionButton); everything else lives in the right-click menu the view
// owns (`menu`).
//
// Sized by the `@container/list` around the list. As it narrows the row
// sheds: extras the slot marks hidden → account label (< 44rem) → time
// (< 38rem; a stale time is never hidden). `compact` squeezes title + meta
// into one line and drops the body.
import { computed } from 'vue'
import { Badge } from '@/components/ui'
import { CircleDot, ExternalLink, GitPullRequest, MessageSquare } from 'lucide-vue-next'

export interface InboxCardAccount {
  label: string
  /** `bg-*` dot class, stable per account. */
  tone: string
  title: string
  /** Further accounts beyond this one (+N). */
  more?: number
}

const props = withDefaults(defineProps<{
  title: string
  kind: 'PullRequest' | 'Issue' | 'Discussion' | string
  owner: string
  repo: string
  number: number | null
  /** Prefix the repo with `owner/` (its bare name is ambiguous in the list). */
  showOwner?: boolean
  /** The group header already names the repo. */
  showRepo?: boolean
  /** Relative time, already formatted. */
  time: string
  stale?: boolean
  staleTitle?: string
  /** Shown only when the group header does not already name the project. */
  projectName?: string | null
  account?: InboxCardAccount | null
  compact?: boolean
  /** Keyboard selection (j / k) — tinted. */
  selected?: boolean
  /** Position in its list, staggers the enter animation. */
  index?: number
  /** Settled / done: dimmed. */
  muted?: boolean
}>(), {
  showOwner: false, showRepo: true, stale: false, staleTitle: '', projectName: null, account: null,
  compact: false, selected: false, index: 0, muted: false,
})

const emit = defineEmits<{
  /** Title clicked: open the item on GitHub. */
  open: []
  /** Any click on the row: it becomes the keyboard selection. */
  select: []
  menu: [x: number, y: number]
}>()

const kindIcon = computed(() =>
  props.kind === 'PullRequest' ? GitPullRequest : props.kind === 'Issue' ? CircleDot : MessageSquare,
)
const kindClass = computed(() =>
  props.kind === 'PullRequest' ? 'text-success' : props.kind === 'Issue' ? 'text-info' : 'text-warning',
)
const repoText = computed(() => (props.showOwner ? `${props.owner}/${props.repo}` : props.repo))
const numberText = computed(() => (props.number !== null ? `#${props.number}` : props.kind === 'Discussion' ? '· Discussion' : ''))
const fullRef = computed(() => `${props.owner}/${props.repo}${props.number !== null ? `#${props.number}` : ''}`)
</script>

<template>
  <div
    class="group relative flex min-w-0 animate-fade-rise transition-colors duration-150"
    :class="[
      compact ? 'items-center gap-2.5 px-3 py-1.5' : 'items-center gap-3 px-3.5 py-2.5',
      selected ? 'bg-primary/10 shadow-[inset_2px_0_0_0_hsl(var(--primary))]' : 'hover:bg-accent/50',
      { 'opacity-70': muted },
    ]"
    :style="{ animationDelay: `${Math.min(index, 12) * 40}ms` }"
    :aria-current="selected || undefined"
    @mousedown="emit('select')"
    @contextmenu.prevent="emit('select'); emit('menu', $event.clientX, $event.clientY)"
  >
    <span class="relative shrink-0 self-start" :class="compact ? '' : 'mt-0.5'">
      <slot name="avatar" />
    </span>

    <!-- Compact: title block grows (floor min-w-24), meta block is capped and
         truncates, so neither collapses nor overlaps the actions. -->
    <div class="min-w-0 flex-1" :class="compact ? 'flex items-center gap-2' : 'flex flex-col gap-1'">
      <div class="flex min-w-0 items-center gap-1.5" :class="{ 'flex-1 min-w-24': compact }">
        <component :is="kindIcon" class="h-3.5 w-3.5 shrink-0" :class="kindClass" :stroke-width="2" />
        <button
          type="button"
          class="min-w-0 truncate text-left font-semibold leading-tight transition-colors hover:text-primary cursor-pointer"
          :class="compact ? 'text-xs' : 'text-sm'"
          :title="title"
          @click="emit('open')"
        >
          {{ title }}
        </button>
        <ExternalLink class="h-3 w-3 shrink-0 text-muted-foreground/50 opacity-0 transition-opacity group-hover:opacity-100" :stroke-width="2" />
        <slot name="badges" />
      </div>

      <div
        class="flex min-w-0 items-center gap-2 text-[11px] text-muted-foreground"
        :class="{ 'max-w-[45%]': compact }"
      >
        <span class="min-w-0 truncate font-mono" :title="fullRef">
          <template v-if="showRepo">{{ repoText }} </template><span class="text-muted-foreground/70">{{ numberText }}</span>
        </span>
        <Badge v-if="projectName" tone="primary" size="xs" class="shrink-0 max-w-32 truncate" :title="projectName">
          {{ projectName }}
        </Badge>
        <slot name="meta" />
      </div>

      <slot v-if="!compact" name="body" />
    </div>

    <!-- Right side: account · time column · primary action -->
    <div class="flex shrink-0 items-center gap-2">
      <span
        v-if="account"
        class="flex shrink-0 items-center gap-1 text-[11px] text-muted-foreground"
        :title="account.title"
        :aria-label="account.title"
        role="img"
      >
        <span class="h-2 w-2 shrink-0 rounded-full" :class="account.tone" />
        <span class="max-w-24 truncate" :class="compact ? 'hidden' : 'hidden @[44rem]/list:inline'">
          {{ account.label }}
        </span>
        <span v-if="account.more" class="text-[10px]">+{{ account.more }}</span>
      </span>
      <!-- Time column: right-aligned, fixed width so rows line up like a
           mail list. Hidden on narrow lists unless stale — staleness is a
           signal worth the space; sr-only text so it isn't colour-only. -->
      <span
        class="w-16 shrink-0 text-right text-[11px] tabular-nums"
        :class="stale ? 'text-warning' : 'hidden @[38rem]/list:inline text-muted-foreground/70'"
        :title="stale ? staleTitle : undefined"
      >
        {{ time }}
        <span v-if="stale" class="sr-only">({{ staleTitle }})</span>
      </span>
      <slot name="action" />
    </div>
  </div>
</template>
