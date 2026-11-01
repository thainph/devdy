<script setup lang="ts">
// On/off switch. Controlled: shows `checked` and emits `toggle`; the parent
// decides what a click does (a saved server applies at once, a draft only
// flips a field).
//
// - `size`: `sm` (h-5, list rows / compact cards) or `md` (h-6, settings rows).
// - `checked: 'mixed'` = some members on (group toggles) — knob in the middle.
import { computed } from 'vue'
import { Loader2 } from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  checked: boolean | 'mixed'
  disabled?: boolean
  /** A spinner in the knob while the change is being applied. */
  loading?: boolean
  label?: string
  size?: 'sm' | 'md'
}>(), {
  disabled: false,
  loading: false,
  label: undefined,
  size: 'sm',
})

const emit = defineEmits<{ toggle: [] }>()

const KNOB = {
  sm: { box: 'h-3.5 w-3.5', on: 'translate-x-[18px]', mixed: 'translate-x-[10.5px]', off: 'translate-x-[3px]' },
  md: { box: 'h-4 w-4', on: 'translate-x-6', mixed: 'translate-x-3.5', off: 'translate-x-1' },
} as const

const state = computed(() => (props.checked === 'mixed' ? 'mixed' : props.checked ? 'on' : 'off'))
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="checked === 'mixed' ? 'mixed' : checked"
    :aria-label="label"
    :title="label"
    :disabled="disabled || loading"
    class="ui-switch relative inline-flex shrink-0 cursor-pointer items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:cursor-not-allowed disabled:opacity-50"
    :class="[
      size === 'md' ? 'h-6 w-11' : 'h-5 w-9',
      state === 'on' ? 'bg-primary' : state === 'mixed' ? 'bg-primary/40' : 'bg-muted-foreground/30',
    ]"
    @click="emit('toggle')"
  >
    <span
      class="ui-switch-knob inline-flex transform items-center justify-center rounded-full bg-white shadow transition-transform"
      :class="[KNOB[size].box, KNOB[size][state]]"
    >
      <Loader2 v-if="loading" :class="size === 'md' ? 'h-3 w-3' : 'h-2.5 w-2.5'" class="animate-spin text-primary" :stroke-width="2.5" />
    </span>
  </button>
</template>
