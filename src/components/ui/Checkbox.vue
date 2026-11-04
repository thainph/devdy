<script setup lang="ts">
// Checkbox with an optional label. `v-model` is a boolean; pass `'mixed'` for a group
// whose members are partly ticked (clicking it emits `true`). The default slot is the
// label: the whole row is the hit area. `tone="destructive"` for a dangerous option
// (e.g. "delete what the source doesn't have").
import { computed } from 'vue'
import { Check, Minus } from 'lucide-vue-next'
import { cn } from '@/lib/utils'

const props = withDefaults(defineProps<{
  modelValue: boolean | 'mixed'
  disabled?: boolean
  tone?: 'primary' | 'destructive'
  /** Accessible name when there is no label slot. */
  label?: string
  class?: string
}>(), {
  disabled: false,
  tone: 'primary',
  label: undefined,
  class: undefined,
})

const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const on = computed(() => props.modelValue !== false)

const boxClass = computed(() =>
  cn(
    'ui-checkbox inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-[3px] border transition-colors',
    !on.value && 'border-muted-foreground/50 bg-background',
    on.value && props.tone === 'primary' && 'border-primary bg-primary text-primary-foreground',
    on.value && props.tone === 'destructive' && 'border-destructive bg-destructive text-destructive-foreground',
    props.modelValue === 'mixed' && 'opacity-70',
  ),
)

function toggle() {
  if (props.disabled) return
  emit('update:modelValue', props.modelValue !== true)
}
</script>

<template>
  <button
    type="button"
    role="checkbox"
    :aria-checked="modelValue === 'mixed' ? 'mixed' : modelValue"
    :aria-label="$slots.default ? undefined : label"
    :disabled="disabled"
    :class="cn(
      'group inline-flex items-start gap-2.5 text-left cursor-pointer focus:outline-none disabled:cursor-not-allowed disabled:opacity-50',
      props.class,
    )"
    @click="toggle"
  >
    <span :class="[boxClass, $slots.default && 'mt-px', 'group-focus-visible:ring-1 group-focus-visible:ring-ring']">
      <Minus v-if="modelValue === 'mixed'" class="h-2.5 w-2.5" :stroke-width="3" />
      <Check v-else-if="modelValue" class="h-2.5 w-2.5" :stroke-width="3" />
    </span>
    <span v-if="$slots.default" class="min-w-0 flex-1">
      <slot />
    </span>
  </button>
</template>
