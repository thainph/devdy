<script setup lang="ts">
// Work Dashboard — one tile per category card, in the cards' order: the
// category, a short answer and a severity stripe (always with the words, never
// colour alone). A tile scrolls to its card.
import { Card } from '@/components/ui'
import type { CategoryKey, Level } from '@/lib/dashboardLayout'
import { LEVEL_STRIPE, LEVEL_TEXT } from './levels'

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
        class="flex w-full cursor-pointer flex-col items-start py-2 pr-3 pl-4 text-left"
        @click="emit('jump', tile.key)"
      >
        <span class="text-[11px] font-medium text-muted-foreground">{{ tile.label }}</span>
        <span class="max-w-full truncate text-sm font-semibold" :class="tile.level === 'ok' || tile.level === 'none' ? 'text-foreground' : LEVEL_TEXT[tile.level]">{{ tile.text }}</span>
      </button>
    </Card>
  </div>
</template>
