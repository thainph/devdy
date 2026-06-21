<script setup lang="ts">
// Shared text input. Matches the app's bg-background + border + focus-ring style.
import { computed, useAttrs } from 'vue'
import { cva, type VariantProps } from 'class-variance-authority'
import { cn } from '@/lib/utils'

defineOptions({ inheritAttrs: false })

const input = cva(
  'w-full bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring transition-colors disabled:opacity-50 disabled:cursor-not-allowed placeholder:text-muted-foreground',
  {
    variants: {
      size: {
        sm: 'px-3 py-2 text-xs',
        md: 'px-3 py-2 text-sm',
      },
    },
    defaultVariants: { size: 'sm' },
  },
)

type InputVariants = VariantProps<typeof input>

const props = withDefaults(defineProps<{
  modelValue?: string | number | null
  size?: InputVariants['size']
  type?: string
  disabled?: boolean
}>(), {
  type: 'text',
  disabled: false,
})

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const attrs = useAttrs()
const classes = computed(() => cn(input({ size: props.size }), attrs.class as string | undefined))
</script>

<template>
  <input
    v-bind="{ ...attrs, class: undefined }"
    :type="type"
    :value="modelValue"
    :disabled="disabled"
    :class="classes"
    @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
  />
</template>
