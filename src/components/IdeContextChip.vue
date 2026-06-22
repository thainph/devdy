<script setup lang="ts">
import { FileCode, TextSelect } from 'lucide-vue-next'
import type { IdeContextItem } from '@/lib/streamEvents'

defineProps<{
  items: IdeContextItem[]
  /** 'inside' tints chips for the user bubble; 'standalone' is the muted row. */
  variant?: 'inside' | 'standalone'
}>()

const emit = defineEmits<{ (e: 'open-file', path: string): void }>()

// Trailing path segment for the compact chip label.
function baseName(path: string): string {
  const clean = path.replace(/[/\\]+$/, '')
  return clean.split(/[/\\]/).pop() || clean
}
</script>

<template>
  <div class="flex flex-col gap-0.5">
    <button
      v-for="(item, ii) in items"
      :key="ii"
      class="group inline-flex items-center gap-1.5 max-w-full text-[11px] transition-colors"
      :class="[
        item.path ? 'cursor-pointer' : 'cursor-default',
        variant === 'inside'
          ? 'text-foreground/55 hover:text-foreground/80'
          : 'text-foreground/45 hover:text-foreground/70',
      ]"
      :title="item.path ? 'Open ' + item.path : undefined"
      :disabled="!item.path"
      @click="item.path && emit('open-file', item.path)"
    >
      <component
        :is="item.kind === 'selection' ? TextSelect : FileCode"
        class="h-3 w-3 shrink-0 opacity-60"
        :stroke-width="1.75"
      />
      <span class="uppercase tracking-wide text-[9px] font-semibold opacity-70">{{
        item.kind === 'selection' ? 'Selected' : 'Opened'
      }}</span>
      <span
        v-if="item.path"
        class="font-mono truncate decoration-dotted underline-offset-2 group-hover:underline group-hover:text-sky-500 dark:group-hover:text-sky-400 transition-colors"
        :class="variant === 'inside' ? 'text-foreground/80' : 'text-foreground/70'"
      >{{ baseName(item.path) }}</span>
      <span v-if="item.detail" class="font-mono opacity-70">{{ item.detail }}</span>
    </button>
  </div>
</template>
