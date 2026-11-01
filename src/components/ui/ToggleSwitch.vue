<script setup lang="ts">
// On/off switch (the same look as the inline ones in the Settings screens).
// Controlled: shows `checked` and emits `toggle`; the parent decides what a
// click does (a saved server applies at once, a draft only flips a field).
import { Loader2 } from 'lucide-vue-next'

defineProps<{
  checked: boolean
  disabled?: boolean
  /** A spinner in the knob while the change is being applied. */
  loading?: boolean
  label?: string
}>()

const emit = defineEmits<{ toggle: [] }>()
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="checked"
    :aria-label="label"
    :title="label"
    :disabled="disabled || loading"
    class="relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:cursor-not-allowed disabled:opacity-50"
    :class="checked ? 'bg-primary' : 'bg-muted-foreground/30'"
    @click="emit('toggle')"
  >
    <span
      class="inline-flex h-3.5 w-3.5 transform items-center justify-center rounded-full bg-white shadow transition-transform"
      :class="checked ? 'translate-x-[18px]' : 'translate-x-[3px]'"
    >
      <Loader2 v-if="loading" class="h-2.5 w-2.5 animate-spin text-primary" :stroke-width="2.5" />
    </span>
  </button>
</template>
