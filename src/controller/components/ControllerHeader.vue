<script setup lang="ts">
/**
 * The top bar every Controller tab and editor shares, so the screens read as
 * one app: an optional leading control (Back), the screen title, an optional
 * badge next to it (connection state) and the actions on the right.
 */
defineProps<{
  title: string
}>()
</script>

<template>
  <header class="app-titlebar shrink-0 flex h-12 items-center gap-2 px-3 border-b border-border">
    <slot name="leading" />
    <h1 class="min-w-0 shrink truncate text-base font-semibold">{{ title }}</h1>
    <slot name="badge" />
    <div class="ml-auto flex min-w-0 shrink-0 items-center gap-1">
      <slot />
    </div>
  </header>
</template>
