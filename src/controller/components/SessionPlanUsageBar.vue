<script setup lang="ts">
/**
 * Controller subscription-usage bar — mirrors the desktop {@link BudgetBadge},
 * but standalone (no Pinia / Tauri). The verdicts arrive from the Host over the
 * relay as a `plan_usage` frame (pushed on pairing and after each turn) and are
 * mirrored into the controller store.
 *
 * Like the desktop badge, this renders ONE ROW PER MANAGED CLAUDE ACCOUNT plus
 * Codex. With several accounts in play the useful question is "which account
 * still has headroom", which a single row for the current run cannot answer.
 * The row belonging to the run in view is marked so it stays findable.
 *
 * An older Host sends no account list; we then fall back to the single `claude`
 * verdict it does send, which is exactly the pre-multi-account behaviour.
 *
 * Read-only: the controller can't trigger a live probe, so there's no refresh
 * button — the number refreshes whenever the Host re-pushes.
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, Dot } from 'lucide-vue-next'
import type { ClaudeAccountUsage, PlanBudget } from '../protocol'

const { t } = useI18n()

const props = defineProps<{
  claude: PlanBudget | null
  codex: PlanBudget | null
  /** Label of the Claude account the focused run is using (null = global). */
  claudeAccount?: string | null
  /** Every managed Claude account. Empty → fall back to `claude` alone. */
  claudeAccounts?: ClaudeAccountUsage[]
}>()

const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null
onMounted(() => {
  now.value = Date.now()
  clock = setInterval(() => { now.value = Date.now() }, 30_000)
})
onUnmounted(() => { if (clock) clearInterval(clock) })

interface Row {
  key: string
  label: string
  /** Full label for the hover tooltip. */
  title: string
  /** Marks the account the run in view actually executes with. */
  active: boolean
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
  if (ms <= 0) return t('controller.planBar.soon')
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? `${days}d${hours}h` : `${days}d`
  if (hours > 0) return minutes > 0 ? `${hours}h${minutes}m` : `${hours}h`
  return `${minutes}m`
}

function rowFor(
  key: string,
  label: string,
  title: string,
  b: PlanBudget | null,
  active = false,
): Row {
  const hasPlan = !!b && b.source === 'plan'
  return {
    key,
    label,
    title,
    active,
    hasPlan,
    percent: b ? Math.min(100, Math.max(0, Math.round(b.percent))) : 0,
    tone: b ? toneOf(b) : 'ok',
    reset: hasPlan ? resetShort(b?.reset) : '',
  }
}

const rows = computed<Row[]>(() => {
  const accounts = props.claudeAccounts ?? []
  const claudeRows = accounts.length
    ? accounts.map((a) =>
        rowFor(
          `claude:${a.id}`,
          a.label,
          a.active
            ? t('controller.planBar.accountActive', { label: a.label })
            : `Claude — ${a.label}`,
          a.budget ?? null,
          a.active,
        ),
      )
    : [
        // Legacy Host (or no managed accounts): one row, labelled with whichever
        // account the Host says the run uses.
        (() => {
          const acct = props.claudeAccount?.trim()
          return rowFor(
            'claude',
            acct || 'Claude',
            acct ? `Claude — ${acct}` : 'Claude',
            props.claude,
          )
        })(),
      ]
  return [...claudeRows, rowFor('codex', 'Codex', 'Codex', props.codex)]
})

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
    class="shrink-0 max-h-24 space-y-1 overflow-y-auto px-3 py-1.5 border-b border-border/60 bg-muted/20"
  >
    <div
      v-for="r in rows"
      :key="r.key"
      class="flex items-center gap-1 text-[10px] leading-none"
    >
      <!-- The run in view executes with this account: a dot, not a colour, so it
           never competes with the warning/over tones. -->
      <Dot
        class="h-3 w-3 shrink-0"
        :class="r.active ? 'text-emerald-500' : 'text-transparent'"
        :stroke-width="6"
      />
      <span
        class="w-16 shrink-0 truncate font-medium"
        :class="r.active ? 'opacity-95' : 'opacity-60'"
        :title="r.title"
      >{{ r.label }}</span>

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

      <span v-else class="flex-1 truncate font-mono opacity-50">{{ t('controller.planBar.noPlanData') }}</span>
    </div>
  </div>
</template>
