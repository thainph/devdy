<script setup lang="ts">
// A table cell's value with a small bar against the column's largest value, so
// the heaviest entries stand out at a glance.
import { computed } from 'vue'

const props = defineProps<{ value: number; max: number; text: string; tone: 'primary' | 'success' | 'warning' }>()
const width = computed(() => (props.max > 0 && props.value > 0 ? Math.max(3, Math.min(100, (props.value / props.max) * 100)) : 0))
const FILL = { primary: 'bg-primary', success: 'bg-success', warning: 'bg-warning' } as const
</script>

<template>
  <div class="flex items-center justify-end gap-2">
    <span class="h-1.5 w-14 shrink-0 overflow-hidden rounded-full bg-muted" aria-hidden="true">
      <span class="block h-full rounded-full transition-[width]" :class="FILL[tone]" :style="{ width: `${width}%` }" />
    </span>
    <span class="w-14 shrink-0 text-right text-[11px] tabular-nums">{{ text }}</span>
  </div>
</template>
