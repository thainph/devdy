<script setup lang="ts">
withDefaults(defineProps<{ disabled?: boolean }>(), { disabled: false })
</script>

<template>
  <button
    type="button"
    :disabled="disabled"
    class="flex w-full items-center gap-2 rounded px-2.5 py-1.5 text-[13px] text-popover-foreground hover:bg-accent transition-colors cursor-pointer text-left disabled:opacity-50 disabled:pointer-events-none"
  >
    <slot />
  </button>
</template>
