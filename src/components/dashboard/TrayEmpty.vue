<script setup lang="ts">
// The empty state inside a Dashboard tray (Running, History): a dashed panel
// that fills the tray's list area, with a title and an optional hint, so both
// trays read the same when there is nothing to list.
defineProps<{ title: string; hint?: string; tone?: 'default' | 'destructive' }>()
</script>

<template>
  <div class="flex flex-1 flex-col items-center justify-center gap-1.5 rounded-[9px] border-[1.5px] border-dashed border-border px-5 py-8 text-center">
    <p class="text-sm font-bold" :class="tone === 'destructive' ? 'text-destructive' : 'text-foreground'">{{ title }}</p>
    <p v-if="hint" class="max-w-[46ch] text-xs text-muted-foreground">{{ hint }}</p>
  </div>
</template>
