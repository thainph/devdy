<script setup lang="ts">
// Activity sparkline: events per minute (oldest first), a line over a soft area
// fill. Turns the warning colour once the last few minutes are empty after
// earlier activity.
import { computed, useId } from 'vue'
import { useI18n } from 'vue-i18n'
import { sparkline } from '@/lib/runtimeDashboard'
import { areaChart } from '@/lib/runtimeCharts'

const props = withDefaults(defineProps<{
  values: readonly number[]
  width?: number
  height?: number
  /** Override the colour (e.g. the CPU line). */
  tone?: string
}>(), { width: 90, height: 20, tone: undefined })

const { t } = useI18n()
const id = useId()
const line = computed(() => sparkline(props.values, 100, 20))
const chart = computed(() => areaChart(props.values, 100, 22, { pad: 2 }))
const color = computed(() => props.tone ?? (line.value.stalled ? 'text-warning' : line.value.empty ? 'text-muted-foreground/50' : 'text-success'))
</script>

<template>
  <svg
    viewBox="0 0 100 22"
    :width="width"
    :height="height"
    preserveAspectRatio="none"
    class="shrink-0"
    :class="color"
    role="img"
    :aria-label="line.stalled ? t('dashboard.activityStalled') : t('dashboard.activityTitle')"
  >
    <title>{{ line.stalled ? t('dashboard.activityStalled') : t('dashboard.activityTitle') }}</title>
    <defs>
      <linearGradient :id="`${id}-spark`" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0%" stop-color="currentColor" stop-opacity="0.3" />
        <stop offset="100%" stop-color="currentColor" stop-opacity="0" />
      </linearGradient>
    </defs>
    <template v-if="!line.empty">
      <path :d="chart.area" :fill="`url(#${id}-spark)`" />
      <path :d="chart.line" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" vector-effect="non-scaling-stroke" />
    </template>
    <line v-else x1="0" y1="20" x2="100" y2="20" stroke="currentColor" stroke-width="1" stroke-dasharray="2 3" vector-effect="non-scaling-stroke" />
  </svg>
</template>
