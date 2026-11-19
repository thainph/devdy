<script setup lang="ts">
// Status dot: pulses while a session streams (static under reduced motion).
import type { SessionState } from '@/lib/runtimeDashboard'
import { STATE_FILL } from './runtimeStyles'

defineProps<{ state: SessionState }>()
</script>

<template>
  <span v-if="state === 'running'" class="relative flex h-2 w-2 shrink-0" aria-hidden="true">
    <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-success opacity-60 motion-reduce:animate-none" />
    <span class="relative inline-flex h-2 w-2 rounded-full bg-success" />
  </span>
  <span v-else class="h-2 w-2 shrink-0 rounded-full" :class="STATE_FILL[state]" aria-hidden="true" />
</template>
