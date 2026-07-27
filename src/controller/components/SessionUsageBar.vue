<script setup lang="ts">
/**
 * Controller usage/context bar — mirrors the desktop ContextMeter + result usage
 * badge, but standalone (no Pinia / appSettings): every value is derived on the
 * controller from the same stream events the desktop uses. Renders:
 *   • a context-window meter (tokens / limit, %),
 *   • a per-turn usage badge (engine · tokens · cost),
 *   • the claude.ai 5-hour window hint when reported.
 */
import { computed } from 'vue'
import { resolveContextLimit, formatTokensShort } from '@/lib/contextLimits'
import type { RateLimitWindows } from '@/stores/liveRuns'
import type { UsageInfo } from '../runStore'

const props = defineProps<{
  /** Context-window occupancy in tokens (0 = nothing yet). */
  tokens: number
  /** Model id from system.init (resolves the context limit). */
  model: string | null
  /** Engine id from the composer selection (`claude` | `codex`). */
  engine: string
  /** claude.ai subscription rate-limit windows, if reported. */
  rateLimit?: RateLimitWindows | null
  /** Latest turn's usage (tokens + cost). */
  usage?: UsageInfo | null
}>()

const WARN_PERCENT = 80

const limit = computed(() => resolveContextLimit(props.model, null))
const ratio = computed(() => (limit.value > 0 ? props.tokens / limit.value : 0))
const percent = computed(() => Math.min(100, Math.round(ratio.value * 100)))
const isWarn = computed(() => percent.value >= WARN_PERCENT)
const isOver = computed(() => ratio.value >= 1)

const barClass = computed(() =>
  isOver.value ? 'bg-red-500' : isWarn.value ? 'bg-amber-500' : 'bg-primary/70',
)
const textClass = computed(() =>
  isOver.value ? 'text-red-500' : isWarn.value ? 'text-amber-500' : 'text-muted-foreground',
)

// claude.ai 5-hour usage window, shown as a secondary hint when available.
const fiveHourText = computed(() => {
  const w = props.rateLimit?.fiveHour
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

const show = computed(() => props.tokens > 0 || !!fiveHourText.value)
</script>

<template>
  <div
    v-if="show"
    class="flex items-center gap-2 px-3 py-1.5 text-[10px] border-t border-border/60 bg-muted/20"
  >
    <!-- Context-window meter -->
    <template v-if="tokens > 0">
      <span class="font-mono shrink-0" :class="textClass">
        {{ formatTokensShort(tokens) }} / {{ formatTokensShort(limit) }}
      </span>
      <div class="relative h-1 flex-1 min-w-12 max-w-40 rounded-full bg-muted overflow-hidden">
        <div
          class="absolute inset-y-0 left-0 rounded-full transition-all"
          :class="barClass"
          :style="{ width: percent + '%' }"
        />
      </div>
      <span class="font-mono shrink-0 tabular-nums" :class="textClass">{{ percent }}%</span>
    </template>

    <!-- claude.ai 5-hour window -->
    <span
      v-if="fiveHourText"
      class="shrink-0 font-mono text-muted-foreground/70 ml-auto"
      title="claude.ai subscription 5-hour usage window"
    >{{ fiveHourText }}</span>
  </div>
</template>
