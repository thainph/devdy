<script setup lang="ts">
/** The floating round "+" action at the bottom-right of a Controller list tab. */
import { Plus } from 'lucide-vue-next'

defineProps<{
  /** Tooltip + accessible name. */
  label: string
  disabled?: boolean
}>()

defineEmits<{ click: [] }>()
</script>

<template>
  <button
    type="button"
    class="absolute right-5 bottom-5 z-10 grid h-14 w-14 place-items-center rounded-full bg-primary text-primary-foreground shadow-lg shadow-primary/40 active:scale-95 transition cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed"
    :title="label"
    :aria-label="label"
    :disabled="disabled"
    @click="$emit('click')"
  >
    <Plus class="h-6 w-6" :stroke-width="2.25" />
  </button>
</template>
