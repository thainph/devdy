<script setup lang="ts">
// One row of a dashboard card: a title and a muted line under it (the reason,
// in words), opening the item on click, with tags and action buttons on the
// right — always visible, so every row can be acted on at once. Title and
// line are cut to one line each; their full text is in the tooltip.
defineProps<{
  title: string
  sub?: string | null
  /** Tooltip of the main button. */
  hint?: string
}>()

const emit = defineEmits<{ open: [] }>()

defineSlots<{
  /** Before the title (status dot, number…). */
  lead?: () => unknown
  /** Tags + action buttons. */
  aside?: () => unknown
}>()
</script>

<template>
  <li class="flex items-center gap-2 px-4 py-1.5 hover:bg-accent/50">
    <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" :title="hint" @click="emit('open')">
      <span class="flex min-w-0 items-center gap-1.5">
        <slot name="lead" />
        <span class="truncate text-[13px] font-medium text-foreground" :title="title">{{ title }}</span>
      </span>
      <span v-if="sub" class="block truncate text-[11px] text-muted-foreground" :title="sub">{{ sub }}</span>
    </button>
    <div v-if="$slots.aside" class="flex shrink-0 items-center gap-1">
      <slot name="aside" />
    </div>
  </li>
</template>
