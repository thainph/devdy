<script setup lang="ts">
// One row of a dashboard card: a title and a muted line under it (the reason,
// in words), opening the item on click, with tags and action buttons on the
// right — always visible, so every row can be acted on at once. A status icon
// (tinted by tone) opens the row. Title and line are cut to one line each;
// their full text is in the tooltip.
import type { Component } from 'vue'
import StatusIcon from './StatusIcon.vue'
import type { Tone } from './levels'

defineProps<{
  title: string
  sub?: string | null
  /** Tooltip of the main button. */
  hint?: string
  /** Status icon at the start of the row. */
  icon?: Component
  tone?: Tone
}>()

const emit = defineEmits<{ open: [] }>()

defineSlots<{
  /** Before the title (status dot, number…). */
  lead?: () => unknown
  /** Rich content for the line under the title (`sub` stays its tooltip). */
  line?: () => unknown
  /** Tags + action buttons. */
  aside?: () => unknown
}>()
</script>

<template>
  <li class="flex items-center gap-2 px-4 py-1.5 hover:bg-accent/50">
    <StatusIcon v-if="icon" :icon="icon" :tone="tone" size="sm" />
    <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" :title="hint" @click="emit('open')">
      <span class="flex min-w-0 items-center gap-1.5">
        <slot name="lead" />
        <span class="truncate text-[13px] font-medium text-foreground" :title="title">{{ title }}</span>
      </span>
      <span v-if="$slots.line" class="flex min-w-0 items-center gap-1 truncate text-[11px] text-muted-foreground" :title="sub ?? undefined"><slot name="line" /></span>
      <span v-else-if="sub" class="block truncate text-[11px] text-muted-foreground" :title="sub">{{ sub }}</span>
    </button>
    <div v-if="$slots.aside" class="flex shrink-0 items-center gap-1">
      <slot name="aside" />
    </div>
  </li>
</template>
