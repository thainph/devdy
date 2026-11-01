<script setup lang="ts">
/**
 * A square icon button for {@link ControllerHeader} (Back, Refresh, Disconnect…)
 * and other bare icon actions in the Controller (composer toolbar).
 */
defineProps<{
  /** Tooltip + accessible name. */
  label: string
  /** Highlight (e.g. notifications on). */
  active?: boolean
  disabled?: boolean
}>()

defineEmits<{ click: [] }>()
</script>

<template>
  <button
    type="button"
    class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md transition-colors cursor-pointer hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed"
    :class="active ? 'text-success' : 'text-foreground/60 hover:text-foreground'"
    :title="label"
    :aria-label="label"
    :disabled="disabled"
    @click="$emit('click')"
  >
    <slot />
  </button>
</template>
