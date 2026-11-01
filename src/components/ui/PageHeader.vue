<script setup lang="ts">
// Top bar of a full-page view (Projects, Stats, Focus, PR Inbox, …): fixed
// h-13 strip with the page title, an optional count pill, free-form content
// after the title (slot `meta`) and right-aligned actions (slot `actions`).
// List-detail screens use `listDetail/DetailHeader` for their right pane instead.
withDefaults(defineProps<{
  title: string
  /** Count pill after the title; hidden when null/undefined. */
  count?: number | string | null
  countTitle?: string
}>(), {
  count: null,
  countTitle: undefined,
})

defineSlots<{
  meta?: () => unknown
  actions?: () => unknown
}>()
</script>

<template>
  <div class="flex h-13 shrink-0 items-center justify-between gap-3 border-b border-border/60 px-6">
    <div class="flex min-w-0 items-center gap-2">
      <h1 class="shrink-0 text-sm font-semibold">{{ title }}</h1>
      <span
        v-if="count !== null && count !== undefined"
        class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        :title="countTitle"
      >
        {{ count }}
      </span>
      <slot name="meta" />
    </div>
    <div v-if="$slots.actions" class="flex shrink-0 items-center gap-2">
      <slot name="actions" />
    </div>
  </div>
</template>
