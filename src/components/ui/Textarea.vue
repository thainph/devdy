<script setup lang="ts">
// Shared multi-line text field. Mirrors Input's look/sizing (from
// controlStyles) plus vertical resize, so textareas stay in sync with inputs
// across every screen.
import { computed, useAttrs } from 'vue'
import { cva, type VariantProps } from 'class-variance-authority'
import { cn } from '@/lib/utils'
import { controlSize, fieldBase } from './controlStyles'

defineOptions({ inheritAttrs: false })

const textarea = cva(`${fieldBase} resize-y`, {
  variants: { size: { ...controlSize } },
  defaultVariants: { size: 'sm' },
})

type TextareaVariants = VariantProps<typeof textarea>

const props = withDefaults(defineProps<{
  modelValue?: string | null
  size?: TextareaVariants['size']
  disabled?: boolean
}>(), {
  disabled: false,
})

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const attrs = useAttrs()
const classes = computed(() => cn(textarea({ size: props.size }), attrs.class as string | undefined))
</script>

<template>
  <textarea
    v-bind="{ ...attrs, class: undefined }"
    :value="modelValue ?? ''"
    :disabled="disabled"
    :class="classes"
    @input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
  />
</template>
