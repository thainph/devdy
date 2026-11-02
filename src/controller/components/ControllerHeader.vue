<script setup lang="ts">
/**
 * The top bar every Controller tab and editor shares, so the screens read as
 * one app: an optional leading control (Back), the screen title, the actions
 * on the right and, in the top-right corner, an optional badge (connection
 * state).
 */
defineProps<{
  title: string
}>()
</script>

<template>
  <header class="app-titlebar shrink-0 flex h-12 items-center gap-2 px-3 border-b border-border">
    <slot name="leading" />
    <h1 class="min-w-0 shrink truncate text-base font-semibold">{{ title }}</h1>
    <div class="ml-auto flex min-w-0 shrink-0 items-center gap-1">
      <slot />
    </div>
    <slot name="badge" />
  </header>
</template>
