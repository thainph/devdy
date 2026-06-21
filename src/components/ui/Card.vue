<script setup lang="ts">
// Panel container used across settings/detail screens. Optional `#header` slot
// renders the standard muted header bar; body content goes in the default slot.
import { computed, useAttrs, useSlots } from 'vue'
import { cn } from '@/lib/utils'

defineOptions({ inheritAttrs: false })

withDefaults(defineProps<{
  /** Extra classes for the body wrapper (e.g. 'p-4' or 'divide-y divide-border/50'). */
  bodyClass?: string
}>(), { bodyClass: '' })

const slots = useSlots()
const attrs = useAttrs()
const rootClass = computed(() =>
  cn('bg-card border border-border rounded-lg overflow-hidden', attrs.class as string | undefined),
)
</script>

<template>
  <div v-bind="{ ...attrs, class: undefined }" :class="rootClass">
    <div v-if="slots.header" class="flex items-center gap-2 px-4 py-3 border-b border-border/60 bg-muted/30">
      <slot name="header" />
    </div>
    <div :class="bodyClass">
      <slot />
    </div>
  </div>
</template>
