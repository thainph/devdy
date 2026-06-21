<script setup lang="ts">
import { computed } from 'vue'
import { resolveContextLimit, formatTokensShort } from '@/lib/contextLimits'
import { useAppSettingsStore } from '@/stores/appSettings'
import type { RateLimitWindows } from '@/stores/liveRuns'

const props = defineProps<{
  /** Tokens currently occupying the context window (0 = nothing yet). */
  tokens: number
  /** Model id from system.init, used to resolve the context limit. */
  model: string | null
  /** Real claude.ai subscription rate-limit windows, if reported. */
  rateLimit?: RateLimitWindows | null
}>()

const emit = defineEmits<{ (e: 'compact'): void }>()

const appSettings = useAppSettingsStore()

const warnPercent = computed(() => {
  const v = Number(appSettings.settings?.context_warn_percent)
  return Number.isFinite(v) && v > 0 ? v : 80
})

const limit = computed(() => {
  const override = Number(appSettings.settings?.context_limit_override)
  return resolveContextLimit(props.model, Number.isFinite(override) ? override : null)
})

const ratio = computed(() => (limit.value > 0 ? props.tokens / limit.value : 0))
const percent = computed(() => Math.min(100, Math.round(ratio.value * 100)))
const isWarn = computed(() => percent.value >= warnPercent.value)
const isOver = computed(() => ratio.value >= 1)

const barClass = computed(() =>
  isOver.value ? 'bg-red-500' : isWarn.value ? 'bg-amber-500' : 'bg-primary/70',
)
const textClass = computed(() =>
  isOver.value ? 'text-red-500' : isWarn.value ? 'text-amber-500' : 'text-muted-foreground',
)

// Real claude.ai 5h window, shown as a secondary hint when available.
const fiveHour = computed(() => props.rateLimit?.fiveHour ?? null)
const fiveHourText = computed(() => {
  const w = fiveHour.value
  if (!w || w.utilization == null) return ''
  let s = `5h ${Math.round(w.utilization)}%`
  if (w.resetsAt) {
    const ms = new Date(w.resetsAt).getTime() - Date.now()
    if (ms > 0) {
      const h = ms / 3_600_000
      s += h >= 1 ? ` · resets ${Math.round(h)}h` : ` · resets ${Math.max(1, Math.round(ms / 60_000))}m`
    }
  }
  return s
})
</script>

<template>
  <div v-if="tokens > 0" class="flex items-center gap-2 px-1 pb-1.5 text-[10px]">
    <span class="font-mono shrink-0" :class="textClass">
      {{ formatTokensShort(tokens) }} / {{ formatTokensShort(limit) }}
    </span>
    <div class="relative h-1 flex-1 min-w-12 max-w-40 rounded-full bg-muted overflow-hidden">
      <div class="absolute inset-y-0 left-0 rounded-full transition-all" :class="barClass" :style="{ width: percent + '%' }" />
    </div>
    <span class="font-mono shrink-0 tabular-nums" :class="textClass">{{ percent }}%</span>
    <button
      v-if="isWarn"
      class="shrink-0 inline-flex items-center gap-1 rounded px-1.5 py-0.5 font-mono cursor-pointer transition-colors text-amber-600 dark:text-amber-400 hover:bg-amber-500/10"
      title="Compact the conversation to free up context (sends /compact)"
      @click="emit('compact')"
    >
      /compact
    </button>
    <span
      v-if="fiveHourText"
      class="shrink-0 font-mono text-muted-foreground/70"
      title="claude.ai subscription 5-hour usage window"
    >· {{ fiveHourText }}</span>
  </div>
</template>
