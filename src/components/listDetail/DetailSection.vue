<script setup lang="ts">
// A flat titled section of a list-detail screen's form (replaces stacked
// Cards): small upper-case heading, optional hint, then the fields.
defineProps<{
  title: string
  hint?: string
}>()

defineSlots<{
  default: () => unknown
  /** Right side of the heading row (e.g. a small action). */
  actions?: () => unknown
}>()
</script>

<template>
  <section class="space-y-3 border-b border-border/60 py-4 last:border-b-0">
    <div class="flex items-center gap-2">
      <h2 class="flex-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{{ title }}</h2>
      <slot name="actions" />
    </div>
    <p v-if="hint" class="-mt-1 text-[11px] text-muted-foreground">{{ hint }}</p>
    <slot />
  </section>
</template>
