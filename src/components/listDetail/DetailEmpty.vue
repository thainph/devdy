<script setup lang="ts">
// Right pane of a list-detail screen when nothing is selected.
import type { Component } from 'vue'

defineProps<{
  icon: Component
  text: string
}>()
</script>

<template>
  <div class="flex min-w-0 flex-1 items-center justify-center p-6">
    <div class="text-center">
      <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
        <component :is="icon" class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-xs text-muted-foreground">{{ text }}</p>
      <slot />
    </div>
  </div>
</template>
