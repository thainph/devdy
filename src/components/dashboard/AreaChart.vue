<script setup lang="ts">
// A small live area chart: a line over a gradient fill in the tone's colour,
// with an optional dot on the latest value. Stretches to its container's width.
import { computed, useId } from 'vue'
import { areaChart } from '@/lib/runtimeCharts'

const props = withDefaults(defineProps<{
  values: readonly number[]
  /** Height in px (the width follows the container). */
  height?: number
  /** Fixed top of the scale; default: follows the values. */
  max?: number
  /** Smallest top of the scale when it follows the values. */
  floor?: number
  tone?: 'primary' | 'success' | 'warning'
  /** Mark the latest value with a dot. */
  dot?: boolean
  /** Accessible description; without it the chart is decorative. */
  label?: string
}>(), { height: 50, max: undefined, floor: 1, tone: 'primary', dot: false, label: '' })

const W = 200
const H = 50
const id = useId()
const chart = computed(() => areaChart(props.values, W, H, { max: props.max, floor: props.floor, pad: 3 }))
const TONE = { primary: 'text-primary', success: 'text-success', warning: 'text-warning' } as const
</script>

<template>
  <div class="relative w-full" :class="TONE[tone]" :style="{ height: `${height}px` }">
    <svg
      :viewBox="`0 0 ${W} ${H}`"
      preserveAspectRatio="none"
      class="block h-full w-full overflow-visible"
      :role="label ? 'img' : undefined"
      :aria-label="label || undefined"
      :aria-hidden="label ? undefined : 'true'"
    >
      <defs>
        <linearGradient :id="`${id}-fill`" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stop-color="currentColor" stop-opacity="0.35" />
          <stop offset="100%" stop-color="currentColor" stop-opacity="0" />
        </linearGradient>
      </defs>
      <line :x1="0" :y1="H - 0.5" :x2="W" :y2="H - 0.5" class="stroke-border" stroke-width="1" vector-effect="non-scaling-stroke" />
      <template v-if="chart.line">
        <path :d="chart.area" :fill="`url(#${id}-fill)`" />
        <path :d="chart.line" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" vector-effect="non-scaling-stroke" />
      </template>
    </svg>
    <span
      v-if="dot && chart.line"
      class="absolute h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-current ring-2 ring-card"
      :style="{ left: `${(chart.lastX / W) * 100}%`, top: `${(chart.lastY / H) * 100}%` }"
      aria-hidden="true"
    />
  </div>
</template>
