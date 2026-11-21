<script setup lang="ts">
// A labelled button for PageHeader actions: icon + label, icon-only while the
// header is narrow (pageHeader.ts). The label stays as title / aria-label.
// The icon goes in the default slot; `after` holds a count pill or similar.
import Button from './Button.vue'
import { pageHeaderCompactButton, pageHeaderLabel } from './pageHeader'

withDefaults(defineProps<{
  label: string
  variant?: 'primary' | 'outline' | 'ghost' | 'destructive' | 'destructive-ghost' | 'subtle'
  disabled?: boolean
}>(), {
  variant: 'outline',
  disabled: false,
})

defineSlots<{
  default?: () => unknown
  after?: () => unknown
}>()
</script>

<template>
  <Button
    :variant="variant"
    size="sm"
    :disabled="disabled"
    :title="label"
    :aria-label="label"
    :class="pageHeaderCompactButton"
  >
    <slot />
    <span :class="pageHeaderLabel">{{ label }}</span>
    <slot name="after" />
  </Button>
</template>
