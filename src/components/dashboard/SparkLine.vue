<script setup lang="ts">
// Activity sparkline: events per minute (oldest first). Turns the warning
// colour once the last few minutes are empty after earlier activity.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { sparkline } from '@/lib/runtimeDashboard'

const props = withDefaults(defineProps<{
  values: readonly number[]
  width?: number
  height?: number
  /** Override the colour (e.g. the CPU line). */
  tone?: string
}>(), { width: 90, height: 20, tone: undefined })

const { t } = useI18n()
const line = computed(() => sparkline(props.values, 100, 20))
const color = computed(() => props.tone ?? (line.value.stalled ? 'text-warning' : line.value.empty ? 'text-muted-foreground/50' : 'text-success'))
</script>

<template>
  <svg
    viewBox="0 0 100 20"
    :width="width"
    :height="height"
    preserveAspectRatio="none"
    class="shrink-0"
    :class="color"
    role="img"
    :aria-label="line.stalled ? t('dashboard.activityStalled') : t('dashboard.activityTitle')"
  >
    <title>{{ line.stalled ? t('dashboard.activityStalled') : t('dashboard.activityTitle') }}</title>
    <polyline
      v-if="line.points"
      :points="line.points"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linejoin="round"
      vector-effect="non-scaling-stroke"
    />
    <line v-else x1="0" y1="18" x2="100" y2="18" stroke="currentColor" stroke-width="1" stroke-dasharray="2 3" vector-effect="non-scaling-stroke" />
  </svg>
</template>
