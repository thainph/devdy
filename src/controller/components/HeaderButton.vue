<script setup lang="ts">
/** A square icon button for {@link ControllerHeader} (Back, Refresh, Disconnect…). */
defineProps<{
  /** Tooltip + accessible name. */
  label: string
  /** Highlight (e.g. notifications on). */
  active?: boolean
}>()

defineEmits<{ click: [] }>()
</script>

<template>
  <button
    type="button"
    class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md transition-colors cursor-pointer hover:bg-accent"
    :class="active ? 'text-emerald-500' : 'text-foreground/60 hover:text-foreground'"
    :title="label"
    :aria-label="label"
    @click="$emit('click')"
  >
    <slot />
  </button>
</template>
