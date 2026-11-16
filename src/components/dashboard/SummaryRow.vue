<script setup lang="ts">
// Work Dashboard — one tile per category card, in the cards' order: the
// category icon tinted by level, the category, a short answer led by the
// level icon, and a severity stripe (always with the words, never colour
// alone). A tile scrolls to its card.
import { Card } from '@/components/ui'
import type { CategoryKey, Level } from '@/lib/dashboardLayout'
import StatusIcon from './StatusIcon.vue'
import { CATEGORY_ICON, LEVEL_ICON, LEVEL_STRIPE, LEVEL_TEXT } from './levels'

export interface SummaryTile {
  key: CategoryKey
  label: string
  text: string
  level: Level
}

defineProps<{ tiles: SummaryTile[] }>()

const emit = defineEmits<{ jump: [key: CategoryKey] }>()
</script>

<template>
  <div class="grid grid-cols-2 gap-3 lg:grid-cols-4">
    <Card v-for="tile in tiles" :key="tile.key" class="relative transition-colors hover:border-primary/40">
      <span class="absolute inset-y-0 left-0 w-1" :class="LEVEL_STRIPE[tile.level]" aria-hidden="true" />
      <button
        type="button"
        class="flex w-full cursor-pointer items-center gap-2.5 py-2 pr-3 pl-4 text-left"
        @click="emit('jump', tile.key)"
      >
        <StatusIcon :icon="CATEGORY_ICON[tile.key]" :tone="tile.level" />
        <span class="flex min-w-0 flex-col">
          <span class="truncate text-[11px] font-medium text-muted-foreground">{{ tile.label }}</span>
          <span
            class="flex min-w-0 items-start gap-1 text-sm leading-snug font-semibold"
            :class="tile.level === 'ok' || tile.level === 'none' ? 'text-foreground' : LEVEL_TEXT[tile.level]"
            :title="tile.text"
          >
            <component
              :is="LEVEL_ICON[tile.level]"
              class="mt-0.5 h-3.5 w-3.5 shrink-0"
              :class="tile.level === 'ok' ? 'text-success' : tile.level === 'none' ? 'text-muted-foreground' : ''"
              :stroke-width="2"
              aria-hidden="true"
            />
            <span class="line-clamp-2 min-w-0">{{ tile.text }}</span>
          </span>
        </span>
      </button>
    </Card>
  </div>
</template>
