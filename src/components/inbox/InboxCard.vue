<script setup lang="ts">
// Shared row of the inbox lists (PR Inbox, GitHub Mentions), so both screens
// read the same: avatar · title row · meta row · optional body · actions.
// Only the primary action is a button (`#actions`); everything else lives in
// the right-click menu, which the view owns (`menu` carries the point).
//
// Sized by the `@container/list` around the list: cards shed meta items at
// the same steps (see `actionLabelClass`). `compact` squeezes title + meta
// into one line and drops the body.
import { Card } from '@/components/ui'
import { ExternalLink } from 'lucide-vue-next'

withDefaults(defineProps<{
  title: string
  compact?: boolean
  /** Inside a group header (slightly tighter padding than the flat list). */
  grouped?: boolean
  /** Position in its list, staggers the enter animation. */
  index?: number
  /** Needs attention: primary bar on the left edge. */
  unread?: boolean
  /** Settled / done: dimmed. */
  muted?: boolean
}>(), { compact: false, grouped: false, index: 0, unread: false, muted: false })

const emit = defineEmits<{
  /** Title clicked: open the item on GitHub. */
  open: []
  menu: [x: number, y: number]
}>()
</script>

<template>
  <Card
    class="group relative animate-fade-rise transition-all duration-150 hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)]"
    :class="{ 'opacity-70': muted }"
    :style="{ animationDelay: `${Math.min(index, 12) * 40}ms` }"
    :body-class="`flex flex-row min-w-0 ${compact ? 'items-center gap-2.5 px-3 py-1.5' : grouped ? 'items-start gap-3 p-3' : 'items-start gap-3 p-4'}`"
    @contextmenu.prevent="emit('menu', $event.clientX, $event.clientY)"
  >
    <span v-if="unread" aria-hidden="true" class="pointer-events-none absolute inset-y-0 left-0 w-[3px] bg-primary" />

    <span class="relative shrink-0">
      <slot name="avatar" />
    </span>

    <!-- Compact: title block grows (floor min-w-24), meta block is capped and
         truncates, so neither collapses nor overlaps the actions. -->
    <div class="min-w-0 flex-1" :class="compact ? 'flex items-center gap-2' : 'flex flex-col gap-1'">
      <div class="flex min-w-0 items-center gap-1.5" :class="{ 'flex-1 min-w-24': compact }">
        <slot name="leading" />
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
        <slot name="meta" />
      </div>

      <slot v-if="!compact" name="body" />
    </div>

    <div class="flex shrink-0 items-center gap-2">
      <slot name="actions" />
    </div>
  </Card>
</template>
