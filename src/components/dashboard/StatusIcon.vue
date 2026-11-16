<script setup lang="ts">
// A lucide icon in a tinted box coloured by tone (severity or accent) — the
// Work Dashboard's status cue, always next to words that say the same thing.
// Theme tokens only, so every theme (and light / dark) recolours it.
import type { Component } from 'vue'
import { TONE_TILE, type Tone } from './levels'

withDefaults(defineProps<{
  icon: Component
  tone?: Tone
  size?: 'sm' | 'md' | 'lg'
  /** Spin the icon (in progress); stops under reduced motion. */
  spin?: boolean
}>(), { tone: 'none', size: 'md', spin: false })

const BOX = { sm: 'h-6 w-6', md: 'h-7 w-7', lg: 'h-9 w-9' } as const
const GLYPH = { sm: 'h-3.5 w-3.5', md: 'h-4 w-4', lg: 'h-5 w-5' } as const
</script>

<template>
  <span
    class="flex shrink-0 items-center justify-center rounded-md"
    :class="[BOX[size], TONE_TILE[tone]]"
    aria-hidden="true"
  >
    <component :is="icon" :class="[GLYPH[size], spin && 'animate-spin motion-reduce:animate-none']" :stroke-width="2" />
  </span>
</template>
