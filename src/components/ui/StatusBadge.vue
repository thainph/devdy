<script setup lang="ts">
// Run-status pill. Maps the app's run statuses to Badge tones and adds the
// pulsing dot for the running state, so every screen renders statuses the same.
import { computed } from 'vue'
import Badge from './Badge.vue'

const props = withDefaults(defineProps<{
  status: string
  size?: 'xs' | 'sm'
}>(), { size: 'sm' })

const TONE: Record<string, 'running' | 'success' | 'error' | 'warning' | 'info' | 'neutral'> = {
  running: 'running',
  done: 'success',
  failed: 'error',
  cancelled: 'warning',
  fetched: 'info',
}

const tone = computed(() => TONE[props.status] ?? 'neutral')
</script>

<template>
  <Badge :tone="tone" :size="size">
    <span v-if="status === 'running'" class="h-1.5 w-1.5 rounded-full bg-current animate-pulse" />
    {{ status }}
  </Badge>
</template>
