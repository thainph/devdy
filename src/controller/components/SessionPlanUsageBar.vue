<script setup lang="ts">
/**
 * Controller subscription-usage bar — mirrors the desktop {@link BudgetBadge}
 * (Claude + Codex plan utilization), but standalone (no Pinia / Tauri). The
 * verdicts arrive from the Host over the relay as a `plan_usage` frame (pushed
 * on pairing and after each turn) and are mirrored into the controller store.
 *
 * Read-only: the controller can't trigger a live probe, so there's no refresh
 * button — the number refreshes whenever the Host re-pushes.
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { AlertTriangle } from 'lucide-vue-next'
import type { PlanBudget } from '../protocol'

const props = defineProps<{
  claude: PlanBudget | null
  codex: PlanBudget | null
}>()

const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null
onMounted(() => {
  now.value = Date.now()
  clock = setInterval(() => { now.value = Date.now() }, 30_000)
})
onUnmounted(() => { if (clock) clearInterval(clock) })

interface Row {
  key: 'claude' | 'codex'
  label: string
  hasPlan: boolean
  percent: number
  tone: 'over' | 'warning' | 'ok'
  reset: string
}

function toneOf(b: PlanBudget): 'over' | 'warning' | 'ok' {
  if (b.is_over) return 'over'
  if (b.is_warning) return 'warning'
  return 'ok'
}

/** Compact reset label, e.g. "5d13h", "2h10m", "32m", "soon". */
function resetShort(iso: string | null | undefined): string {
  if (!iso) return ''
  const ms = new Date(iso).getTime() - now.value
  if (ms <= 0) return 'soon'
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? `${days}d${hours}h` : `${days}d`
  if (hours > 0) return minutes > 0 ? `${hours}h${minutes}m` : `${hours}h`
  return `${minutes}m`
}

function rowFor(key: 'claude' | 'codex', label: string, b: PlanBudget | null): Row {
  const hasPlan = !!b && b.source === 'plan'
  return {
    key,
    label,
    hasPlan,
    percent: b ? Math.min(100, Math.max(0, Math.round(b.percent))) : 0,
    tone: b ? toneOf(b) : 'ok',
    reset: hasPlan ? resetShort(b?.reset) : '',
  }
}

const rows = computed<Row[]>(() => [
  rowFor('claude', 'Claude', props.claude),
  rowFor('codex', 'Codex', props.codex),
])

// Show the bar only once at least one provider reports real plan usage.
const show = computed(() => rows.value.some((r) => r.hasPlan))

const FILL: Record<Row['tone'], string> = {
  over: 'bg-red-500',
  warning: 'bg-amber-500',
  ok: 'bg-indigo-500',
}
const TEXT: Record<Row['tone'], string> = {
  over: 'text-red-500',
  warning: 'text-amber-500',
  ok: 'text-foreground',
}
</script>

<template>
  <div
    v-if="show"
    class="shrink-0 space-y-1 px-3 py-1.5 border-b border-border/60 bg-muted/20"
  >
    <div
      v-for="r in rows"
      :key="r.key"
      class="flex items-center gap-2 text-[10px] leading-none"
    >
      <span class="w-11 shrink-0 font-medium opacity-70">{{ r.label }}</span>

      <template v-if="r.hasPlan">
        <AlertTriangle
          v-if="r.tone !== 'ok'"
          class="h-3 w-3 shrink-0"
          :class="TEXT[r.tone]"
          :stroke-width="2.5"
        />
        <span
          class="w-8 shrink-0 text-right font-mono font-semibold tabular-nums"
          :class="TEXT[r.tone]"
        >{{ r.percent }}%</span>
        <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-black/10 dark:bg-white/10">
          <div
            class="h-full rounded-full transition-[width] duration-300"
            :class="FILL[r.tone]"
            :style="{ width: r.percent + '%' }"
          />
        </div>
        <span class="w-11 shrink-0 text-right font-mono tabular-nums opacity-60">{{ r.reset }}</span>
      </template>

      <span v-else class="flex-1 truncate font-mono opacity-50">chưa có dữ liệu gói</span>
    </div>
  </div>
</template>
