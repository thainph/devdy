<script setup lang="ts">
// Shared text input. Matches the app's bg-background + border + focus-ring style.
import { computed, useAttrs } from 'vue'
import { cva, type VariantProps } from 'class-variance-authority'
import { cn } from '@/lib/utils'
import { controlSize, fieldBase } from './controlStyles'

defineOptions({ inheritAttrs: false })

const input = cva(fieldBase, {
  variants: { size: { ...controlSize } },
  defaultVariants: { size: 'sm' },
})

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
