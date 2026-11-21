<script setup lang="ts">
// Top bar of a full-page view (Projects, Stats, Focus, PR Inbox, …): fixed
// h-13 strip with the page title, an optional count pill, free-form content
// after the title (slot `meta`) and right-aligned actions (slot `actions`).
// List-detail screens use `listDetail/DetailHeader` for their right pane instead.
//
// Any screen can sit in half of a split, so the header sizes against itself
// (`@container/page-header`), not the window: the title shrinks with an
// ellipsis, and below 40rem the padding drops to px-3 and the actions go
// compact — PageHeaderButton / pageHeaderLabel hide button labels,
// PageHeaderSearch folds to an icon (see pageHeader.ts).
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
  /** Before the title (back button, page icon). */
  leading?: () => unknown
  meta?: () => unknown
  actions?: () => unknown
}>()
</script>

<template>
  <div class="@container/page-header shrink-0 border-b border-border/60">
    <div class="relative flex h-13 items-center justify-between gap-3 px-3 @[40rem]/page-header:px-6">
      <div class="flex min-w-0 items-center gap-2">
        <slot name="leading" />
        <h1 class="min-w-0 truncate text-sm font-semibold" :title="title">
          {{ title }}
        </h1>
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
  </div>
</template>
