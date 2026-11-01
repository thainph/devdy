<script setup lang="ts" generic="T extends string | number">
// Pill-style switcher (filters, view modes, auth method…): a muted track with
// the active segment lifted on a background chip. Replaces the hand-copied
// `rounded-md bg-muted p-0.5` + per-button class blocks.
//
// - `stretch` (default) spreads segments over the full width (sidebar filters).
// - `iconOnly` renders just the icon; `label` becomes the tooltip/aria-label.
import type { Component } from 'vue'
import { cn } from '@/lib/utils'

export interface SegmentedOption<V> {
  value: V
  label: string
  icon?: Component
  /** Tooltip; defaults to `label` when `iconOnly`. */
  title?: string
}

const props = withDefaults(defineProps<{
  modelValue: T
  options: SegmentedOption<T>[]
  stretch?: boolean
  iconOnly?: boolean
  class?: string
}>(), {
  stretch: true,
  iconOnly: false,
  class: undefined,
})

const emit = defineEmits<{ 'update:modelValue': [value: T] }>()
</script>

<template>
  <div
    :class="cn('flex rounded-md bg-muted p-0.5 text-[11px] font-medium', !stretch && 'shrink-0', props.class)"
    role="group"
  >
    <button
      v-for="o in options"
      :key="String(o.value)"
      type="button"
      class="flex cursor-pointer items-center justify-center gap-1 rounded transition-colors"
      :class="[
        stretch && 'flex-1',
        iconOnly ? 'p-1' : 'px-2 py-1',
        modelValue === o.value
          ? 'bg-background text-foreground shadow-sm'
          : 'text-muted-foreground hover:text-foreground',
      ]"
      :title="o.title ?? (iconOnly ? o.label : undefined)"
      :aria-label="iconOnly ? o.label : undefined"
      :aria-pressed="modelValue === o.value"
      @click="emit('update:modelValue', o.value)"
    >
      <component :is="o.icon" v-if="o.icon" :class="iconOnly ? 'h-3.5 w-3.5' : 'h-3 w-3'" :stroke-width="iconOnly ? 1.75 : 2" />
      <template v-if="!iconOnly">{{ o.label }}</template>
    </button>
  </div>
</template>
