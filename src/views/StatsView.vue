<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { invoke } from '@/lib/tauri'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  Chart as ChartJS,
  Title,
  Tooltip,
  Legend,
  BarController,
  BarElement,
  LineController,
  LineElement,
  PointElement,
  DoughnutController,
  ArcElement,
  CategoryScale,
  LinearScale,
} from 'chart.js'
import { Bar, Doughnut } from 'vue-chartjs'
import {
  Coins, DollarSign, Play, Repeat, RefreshCw, Trash2, Loader2, Info, AlertTriangle, HardDrive, Gauge,
} from 'lucide-vue-next'
import { Button, Input, Card, AppSelect } from '@/components/ui'
import { useProjectsStore } from '@/stores/projects'
import {
  getUsageStats, backfillUsage, resetUsageStats,
  getStorageStats, cleanStorage,
  type StatsResult, type StatsFilter, type StorageStats, type StorageCategory,
} from '@/stores/stats'

// Register controllers explicitly. The mixed bar/line daily chart needs both
// BarController and LineController. Relying on vue-chartjs's import side effects
// (createTypedChart calls Chart.register) breaks in production: those calls are
// marked /* #__PURE__ */, so Rollup tree-shakes away the unused Line component
// and LineController never registers — the daily chart then fails only in builds.
ChartJS.register(
  Title, Tooltip, Legend,
  BarController, BarElement,
  LineController, LineElement, PointElement,
  DoughnutController, ArcElement,
  CategoryScale, LinearScale,
)

const projectsStore = useProjectsStore()

// ── filters ────────────────────────────────────────────────────────────────
const RANGE_OPTIONS = [
  { value: '7', label: 'Last 7 days' },
  { value: '30', label: 'Last 30 days' },
  { value: '90', label: 'Last 90 days' },
  { value: 'all', label: 'All time' },
]
const range = ref('30')
const engine = ref('') // '' = all
const projectId = ref('') // '' = all

const engineOptions = [
  { value: '', label: 'All engines' },
  { value: 'claude', label: 'Claude' },
  { value: 'codex', label: 'Codex' },
]
const projectOptions = computed(() => [
  { value: '', label: 'All projects' },
  ...projectsStore.projects.map((p) => ({ value: p.id, label: p.name })),
])

function rangeToDates(r: string): { from: string | null; to: string | null } {
  if (r === 'all') return { from: null, to: null }
  const days = parseInt(r, 10)
  const now = new Date()
  const start = new Date(now)
  start.setDate(now.getDate() - (days - 1))
  const fmt = (d: Date) => d.toISOString().slice(0, 10)
  return { from: fmt(start), to: fmt(now) }
}

const currentFilter = computed<StatsFilter>(() => {
  const { from, to } = rangeToDates(range.value)
  return {
    from,
    to,
    engine: engine.value || null,
    project_id: projectId.value || null,
  }
})

// ── data ─────────────────────────────────────────────────────────────────────
const stats = ref<StatsResult | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

async function load() {
  loading.value = true
  error.value = null
  try {
    stats.value = await getUsageStats(currentFilter.value)
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
}

watch(currentFilter, load, { deep: true })

onMounted(async () => {
  if (projectsStore.projects.length === 0) await projectsStore.fetchProjects()
  now.value = Date.now()
  clockTimer = setInterval(() => { now.value = Date.now() }, 30_000)
  await Promise.all([load(), loadStorage(), refreshPlanUsage(), refreshPlanUsageCodex()])
  unlistenPlanUsage = await listen<{ provider?: string }>('plan_usage_updated', (e) => {
    if (e.payload?.provider === 'codex') {
      refreshPlanUsageCodex().catch(() => {})
    } else {
      refreshPlanUsage().catch(() => {})
    }
  })
})

onUnmounted(() => {
  if (clockTimer) clearInterval(clockTimer)
  if (unlistenPlanUsage) unlistenPlanUsage()
})

// ── formatting ────────────────────────────────────────────────────────────────
function fmtNum(n: number): string {
  return n.toLocaleString('en-US')
}
function fmtCompact(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M'
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k'
  return String(n)
}
function fmtCost(n: number): string {
  if (n === 0) return '$0'
  if (n < 0.01) return '$' + n.toFixed(4)
  return '$' + n.toFixed(2)
}

const hasData = computed(() => (stats.value?.summary.total_turns ?? 0) > 0)

// ── chart palette ────────────────────────────────────────────────────────────
const INDIGO = '#6366f1'
const AMBER = '#f59e0b'
const PALETTE = ['#6366f1', '#22c55e', '#f59e0b', '#ec4899', '#06b6d4', '#a855f7', '#ef4444', '#14b8a6']

const gridColor = 'rgba(127,127,127,0.12)'
const tickColor = 'rgba(148,148,160,0.9)'

// Daily tokens (bars) + cost (line, right axis).
// Typed `any` because this is a mixed bar/line dataset, which doesn't fit
// vue-chartjs's `ChartData<'bar'>` prop type.
const dailyChartData = computed((): any => {
  const d = stats.value?.daily ?? []
  return {
    labels: d.map((p) => p.date.slice(5)), // MM-DD
    datasets: [
      {
        type: 'bar' as const,
        label: 'Tokens',
        data: d.map((p) => p.tokens),
        backgroundColor: 'rgba(99,102,241,0.55)',
        borderColor: INDIGO,
        borderWidth: 1,
        borderRadius: 3,
        yAxisID: 'y',
        order: 2,
      },
      {
        type: 'line' as const,
        label: 'Cost (USD)',
        data: d.map((p) => p.cost),
        borderColor: AMBER,
        backgroundColor: AMBER,
        borderWidth: 2,
        pointRadius: 2,
        tension: 0.3,
        yAxisID: 'y1',
        order: 1,
      },
    ],
  }
})

const dailyChartOptions = {
  responsive: true,
  maintainAspectRatio: false,
  interaction: { mode: 'index' as const, intersect: false },
  plugins: {
    legend: { labels: { color: tickColor, boxWidth: 12, font: { size: 11 } } },
  },
  scales: {
    x: { grid: { color: gridColor }, ticks: { color: tickColor, font: { size: 10 }, maxRotation: 0, autoSkip: true } },
    y: {
      position: 'left' as const,
      grid: { color: gridColor },
      ticks: { color: tickColor, font: { size: 10 }, callback: (v: number | string) => fmtCompact(Number(v)) },
    },
    y1: {
      position: 'right' as const,
      grid: { drawOnChartArea: false },
      ticks: { color: AMBER, font: { size: 10 }, callback: (v: number | string) => '$' + Number(v).toFixed(2) },
    },
  },
}

const engineChartData = computed(() => {
  const e = stats.value?.by_engine ?? []
  return {
    labels: e.map((x) => x.engine),
    datasets: [
      {
        data: e.map((x) => x.tokens),
        backgroundColor: e.map((_, i) => PALETTE[i % PALETTE.length]),
        borderWidth: 0,
      },
    ],
  }
})

const doughnutOptions = {
  responsive: true,
  maintainAspectRatio: false,
  cutout: '62%',
  plugins: {
    legend: { position: 'bottom' as const, labels: { color: tickColor, boxWidth: 12, font: { size: 11 } } },
  },
}

function pct(part: number, total: number): string {
  if (!total) return '0%'
  return ((part / total) * 100).toFixed(0) + '%'
}

// ── real subscription plan usage (read live from /usage during runs) ────────────
// The budget store only holds the single guardrail verdict, so Stats fetches the
// raw `/usage` snapshot itself for the detailed breakdown below.
interface PlanWindowData { utilization: number | null; resets_at: string | null }
interface PlanUsageData {
  subscription_type: string | null
  rate_limits_available: boolean
  windows: Record<'five_hour' | 'seven_day' | 'seven_day_opus' | 'seven_day_sonnet', PlanWindowData>
}
const planUsage = ref<PlanUsageData | null>(null)
const planUsageCodex = ref<PlanUsageData | null>(null)
const now = ref(Date.now())
let unlistenPlanUsage: UnlistenFn | null = null
let clockTimer: ReturnType<typeof setInterval> | null = null

async function refreshPlanUsage() {
  planUsage.value = await invoke<PlanUsageData | null>('get_plan_usage')
}
async function refreshPlanUsageCodex() {
  planUsageCodex.value = await invoke<PlanUsageData | null>('get_plan_usage_codex')
}

const PLAN_WINDOWS: { key: 'five_hour' | 'seven_day' | 'seven_day_opus' | 'seven_day_sonnet'; label: string }[] = [
  { key: 'five_hour', label: 'Current session (5h)' },
  { key: 'seven_day', label: 'This week (all models)' },
  { key: 'seven_day_opus', label: 'This week (Opus)' },
  { key: 'seven_day_sonnet', label: 'This week (Sonnet)' },
]
function planRowsFor(u: PlanUsageData | null, windows: typeof PLAN_WINDOWS) {
  if (!u || !u.rate_limits_available) return []
  return windows.map((w) => {
    const win = u.windows[w.key]
    return {
      label: w.label,
      utilization: win?.utilization ?? null,
      resets_at: win?.resets_at ?? null,
    }
  }).filter((r) => r.utilization != null)
}
const planRows = computed(() => planRowsFor(planUsage.value, PLAN_WINDOWS))
// Codex only reports two windows: rolling 5h + weekly (no per-model split).
const CODEX_PLAN_WINDOWS: typeof PLAN_WINDOWS = [
  { key: 'five_hour', label: 'Current session (5h)' },
  { key: 'seven_day', label: 'This week' },
]
const planRowsCodex = computed(() => planRowsFor(planUsageCodex.value, CODEX_PLAN_WINDOWS))
function planResetText(iso: string | null): string {
  if (!iso) return ''
  const ms = new Date(iso).getTime() - now.value
  if (ms <= 0) return 'resets soon'
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? `resets in ${days}d ${hours}h` : `resets in ${days}d`
  if (hours > 0) return minutes > 0 ? `resets in ${hours}h ${minutes}m` : `resets in ${hours}h`
  return `resets in ${minutes}m`
}

// ── actions: backfill + reset ──────────────────────────────────────────────────
const backfilling = ref(false)
const actionMsg = ref<string | null>(null)

async function doBackfill() {
  backfilling.value = true
  actionMsg.value = null
  try {
    const r = await backfillUsage()
    actionMsg.value = `Backfilled ${r.inserted} record(s) from ${r.runs_scanned} run log(s).`
    await load()
  } catch (e) {
    actionMsg.value = `Backfill failed: ${e}`
  } finally {
    backfilling.value = false
  }
}

const showReset = ref(false)
const resetConfirm = ref('')
const resetting = ref(false)

function openReset() {
  resetConfirm.value = ''
  showReset.value = true
}

async function doReset() {
  if (resetConfirm.value !== 'RESET') return
  resetting.value = true
  try {
    const r = await resetUsageStats()
    actionMsg.value = `Reset done — deleted ${r.runs_deleted} run(s), cleared ${r.usage_cleared} usage record(s).`
    showReset.value = false
    await load()
  } catch (e) {
    actionMsg.value = `Reset failed: ${e}`
  } finally {
    resetting.value = false
  }
}

// ── storage: disk usage + log cleanup ──────────────────────────────────────────
const storage = ref<StorageStats | null>(null)
const storageLoading = ref(false)
const cleaningId = ref<string | null>(null)
const cleanTarget = ref<StorageCategory | null>(null)

function fmtBytes(n: number): string {
  if (n <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.min(Math.floor(Math.log(n) / Math.log(1024)), units.length - 1)
  const v = n / Math.pow(1024, i)
  return `${v.toFixed(i === 0 ? 0 : v >= 100 ? 0 : 1)} ${units[i]}`
}

async function loadStorage() {
  storageLoading.value = true
  try {
    storage.value = await getStorageStats()
  } catch (e) {
    actionMsg.value = `Failed to read storage: ${e}`
  } finally {
    storageLoading.value = false
  }
}

async function doClean() {
  const cat = cleanTarget.value
  if (!cat) return
  cleaningId.value = cat.id
  try {
    const r = await cleanStorage(cat.id)
    actionMsg.value = `Cleaned ${cat.label} — removed ${fmtNum(r.deleted_files)} file(s), freed ${fmtBytes(r.freed_bytes)}.`
    cleanTarget.value = null
    await loadStorage()
  } catch (e) {
    actionMsg.value = `Cleanup failed: ${e}`
  } finally {
    cleaningId.value = null
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <h1 class="text-sm font-semibold">Usage Stats</h1>
      <div class="flex items-center gap-2">
        <Button
          variant="outline"
          :disabled="backfilling"
          title="Rebuild usage records from existing run logs"
          @click="doBackfill"
        >
          <Loader2 v-if="backfilling" class="h-3.5 w-3.5 animate-spin" />
          <RefreshCw v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
          Backfill from logs
        </Button>
        <Button
          variant="destructive"
          title="Delete all runs and reset counters to zero"
          @click="openReset"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          Reset
        </Button>
      </div>
    </div>

    <!-- Body -->
    <div class="flex-1 overflow-auto p-6 space-y-5">
      <!-- Filters -->
      <div class="flex flex-wrap items-center gap-2">
        <AppSelect v-model="range" :options="RANGE_OPTIONS" size="sm" class="w-40" />
        <AppSelect v-model="engine" :options="engineOptions" size="sm" class="w-40" />
        <AppSelect v-model="projectId" :options="projectOptions" size="sm" class="w-52" />
        <span v-if="loading" class="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Loader2 class="h-3.5 w-3.5 animate-spin" /> Loading…
        </span>
      </div>

      <p v-if="actionMsg" class="text-xs text-muted-foreground">{{ actionMsg }}</p>
      <p v-if="error" class="text-xs text-red-500">{{ error }}</p>

      <!-- Real subscription plan usage (independent of token stats) -->
      <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <!-- Claude subscription plan usage (from /usage) -->
        <Card class="border-border/60" body-class="p-4 space-y-3">
          <div>
            <h2 class="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
              <Gauge class="h-3.5 w-3.5" :stroke-width="1.75" /> Plan usage (Claude subscription)
            </h2>
            <p class="text-[11px] text-muted-foreground/70 leading-relaxed mt-1">
              Your account's <b>real</b> usage and reset times, read live from Claude's
              <code class="font-mono bg-muted px-1 rounded">/usage</code> data during runs.
            </p>
          </div>
          <div v-if="planRows.length" class="space-y-2">
            <div v-for="row in planRows" :key="row.label" class="space-y-1">
              <div class="flex items-baseline justify-between text-[11px]">
                <span class="font-medium">{{ row.label }}</span>
                <span class="font-mono text-muted-foreground">
                  {{ Math.round(row.utilization!) }}%
                  <span v-if="planResetText(row.resets_at)"> · {{ planResetText(row.resets_at) }}</span>
                </span>
              </div>
              <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                <div
                  class="h-full rounded-full transition-all"
                  :class="row.utilization! >= 100 ? 'bg-red-500' : row.utilization! >= 80 ? 'bg-amber-500' : 'bg-indigo-500'"
                  :style="{ width: Math.min(100, Math.round(row.utilization!)) + '%' }"
                />
              </div>
            </div>
            <p v-if="planUsage?.subscription_type" class="text-[11px] text-muted-foreground">
              Plan: <b class="capitalize">{{ planUsage.subscription_type }}</b>
            </p>
          </div>
          <p v-else class="text-[11px] text-muted-foreground italic">
            No plan usage captured yet — run a Claude task and it will populate here.
            (Unavailable for API-key / non-subscription sessions.)
          </p>
        </Card>

        <!-- Codex subscription plan usage (from account/rateLimits) -->
        <Card class="border-border/60" body-class="p-4 space-y-3">
          <div>
            <h2 class="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
              <Gauge class="h-3.5 w-3.5" :stroke-width="1.75" /> Plan usage (Codex subscription)
            </h2>
            <p class="text-[11px] text-muted-foreground/70 leading-relaxed mt-1">
              Your Codex account's <b>real</b> rate-limit usage (the numbers behind
              <code class="font-mono bg-muted px-1 rounded">/status</code>), updated after each Codex run.
            </p>
          </div>
          <div v-if="planRowsCodex.length" class="space-y-2">
            <div v-for="row in planRowsCodex" :key="row.label" class="space-y-1">
              <div class="flex items-baseline justify-between text-[11px]">
                <span class="font-medium">{{ row.label }}</span>
                <span class="font-mono text-muted-foreground">
                  {{ Math.round(row.utilization!) }}%
                  <span v-if="planResetText(row.resets_at)"> · {{ planResetText(row.resets_at) }}</span>
                </span>
              </div>
              <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                <div
                  class="h-full rounded-full transition-all"
                  :class="row.utilization! >= 100 ? 'bg-red-500' : row.utilization! >= 80 ? 'bg-amber-500' : 'bg-indigo-500'"
                  :style="{ width: Math.min(100, Math.round(row.utilization!)) + '%' }"
                />
              </div>
            </div>
            <p v-if="planUsageCodex?.subscription_type" class="text-[11px] text-muted-foreground">
              Plan: <b class="capitalize">{{ planUsageCodex.subscription_type }}</b>
            </p>
          </div>
          <p v-else class="text-[11px] text-muted-foreground italic">
            No Codex plan usage captured yet — run a Codex task and it will populate here.
          </p>
        </Card>
      </div>

      <!-- Empty state -->
      <div
        v-if="!loading && !hasData"
        class="flex flex-col items-center justify-center py-20 text-center"
      >
        <Coins class="h-8 w-8 text-muted-foreground/40 mb-3" :stroke-width="1.5" />
        <p class="text-sm text-muted-foreground">No usage recorded for this filter.</p>
        <p class="text-xs text-muted-foreground/70 mt-1">
          Run an analysis, or click "Backfill from logs" to import past runs.
        </p>
      </div>

      <template v-if="hasData && stats">
        <!-- Summary cards -->
        <div class="grid grid-cols-2 lg:grid-cols-4 gap-3">
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Coins class="h-3.5 w-3.5" :stroke-width="1.75" /> Total tokens
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtNum(stats.summary.total_tokens) }}</p>
            <p class="text-[11px] text-muted-foreground/70 mt-1">
              in {{ fmtCompact(stats.summary.total_input) }} · out {{ fmtCompact(stats.summary.total_output) }}
              · cache {{ fmtCompact(stats.summary.total_cache) }}
            </p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <DollarSign class="h-3.5 w-3.5" :stroke-width="1.75" /> Est. cost
              <span title="Quy đổi tương đương API. Tài khoản dùng subscription nên chi phí thực tế là phí thuê bao cố định, không tính theo token.">
                <Info class="h-3 w-3 text-muted-foreground/50" />
              </span>
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtCost(stats.summary.total_cost) }}</p>
            <p class="text-[11px] text-muted-foreground/70 mt-1">
              {{ fmtCost(stats.summary.estimated_cost) }} estimated
            </p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Play class="h-3.5 w-3.5" :stroke-width="1.75" /> Runs
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtNum(stats.summary.total_runs) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Repeat class="h-3.5 w-3.5" :stroke-width="1.75" /> Turns
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtNum(stats.summary.total_turns) }}</p>
          </Card>
        </div>

        <!-- Daily chart -->
        <Card class="border-border/60" body-class="p-4">
          <h2 class="text-xs font-medium text-muted-foreground mb-3">Daily tokens &amp; cost</h2>
          <div class="h-64">
            <Bar :data="dailyChartData" :options="dailyChartOptions" />
          </div>
        </Card>

        <!-- Engine + project breakdown -->
        <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <Card class="border-border/60" body-class="p-4">
            <h2 class="text-xs font-medium text-muted-foreground mb-3">By engine</h2>
            <div class="h-52">
              <Doughnut :data="engineChartData" :options="doughnutOptions" />
            </div>
            <table class="w-full mt-3 text-xs">
              <tbody>
                <tr v-for="(e, i) in stats.by_engine" :key="e.engine" class="border-t border-border/40">
                  <td class="py-1.5">
                    <span class="inline-block h-2.5 w-2.5 rounded-sm mr-1.5 align-middle"
                      :style="{ background: PALETTE[i % PALETTE.length] }" />
                    {{ e.engine }}
                  </td>
                  <td class="py-1.5 text-right tabular-nums">{{ fmtNum(e.tokens) }}</td>
                  <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ fmtCost(e.cost) }}</td>
                </tr>
              </tbody>
            </table>
          </Card>

          <Card class="border-border/60" body-class="p-4">
            <h2 class="text-xs font-medium text-muted-foreground mb-3">By project</h2>
            <table class="w-full text-xs">
              <thead>
                <tr class="text-muted-foreground/70 text-left">
                  <th class="font-normal pb-2">Project</th>
                  <th class="font-normal pb-2 text-right">Tokens</th>
                  <th class="font-normal pb-2 text-right">Cost</th>
                  <th class="font-normal pb-2 text-right">%</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="p in stats.by_project" :key="p.project_id ?? 'none'" class="border-t border-border/40">
                  <td class="py-1.5 truncate max-w-[160px]">{{ p.project_name ?? '(deleted)' }}</td>
                  <td class="py-1.5 text-right tabular-nums">{{ fmtNum(p.tokens) }}</td>
                  <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ fmtCost(p.cost) }}</td>
                  <td class="py-1.5 text-right tabular-nums text-muted-foreground">
                    {{ pct(p.tokens, stats.summary.total_tokens) }}
                  </td>
                </tr>
              </tbody>
            </table>
          </Card>
        </div>

        <!-- By model -->
        <Card class="border-border/60" body-class="p-4">
          <h2 class="text-xs font-medium text-muted-foreground mb-3">By model</h2>
          <table class="w-full text-xs">
            <thead>
              <tr class="text-muted-foreground/70 text-left">
                <th class="font-normal pb-2">Model</th>
                <th class="font-normal pb-2 text-right">Tokens</th>
                <th class="font-normal pb-2 text-right">Cost</th>
                <th class="font-normal pb-2 text-right">Runs</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="m in stats.by_model" :key="m.model ?? 'unknown'" class="border-t border-border/40">
                <td class="py-1.5 font-mono">{{ m.model ?? '(unknown)' }}</td>
                <td class="py-1.5 text-right tabular-nums">{{ fmtNum(m.tokens) }}</td>
                <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ fmtCost(m.cost) }}</td>
                <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ fmtNum(m.runs) }}</td>
              </tr>
            </tbody>
          </table>
        </Card>
      </template>

      <!-- Storage: disk usage + session/run log cleanup (independent of token data) -->
      <Card class="border-border/60" body-class="p-4">
        <div class="flex items-center justify-between mb-3">
          <h2 class="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
            <HardDrive class="h-3.5 w-3.5" :stroke-width="1.75" /> Storage — session/run logs
            <span v-if="storage" class="text-muted-foreground/60">
              · {{ fmtBytes(storage.total_bytes) }} total
            </span>
          </h2>
          <Button variant="ghost" size="sm" :disabled="storageLoading" @click="loadStorage">
            <Loader2 v-if="storageLoading" class="h-3.5 w-3.5 animate-spin" />
            <RefreshCw v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
            Rescan
          </Button>
        </div>

        <div v-if="!storage && storageLoading" class="text-xs text-muted-foreground py-4">
          Scanning disk…
        </div>

        <table v-else-if="storage" class="w-full text-xs">
          <thead>
            <tr class="text-muted-foreground/70 text-left">
              <th class="font-normal pb-2">Category</th>
              <th class="font-normal pb-2 text-right">Files</th>
              <th class="font-normal pb-2 text-right">Size</th>
              <th class="font-normal pb-2 text-right">%</th>
              <th class="font-normal pb-2 text-right"></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="c in storage.categories" :key="c.id" class="border-t border-border/40">
              <td class="py-2">
                <div class="flex items-center gap-1.5">
                  <span>{{ c.label }}</span>
                  <span v-if="c.destructive" :title="c.description">
                    <AlertTriangle class="h-3 w-3 text-amber-500" />
                  </span>
                  <span v-else :title="c.description">
                    <Info class="h-3 w-3 text-muted-foreground/50" />
                  </span>
                </div>
                <p class="font-mono text-[10px] text-muted-foreground/60 mt-0.5">{{ c.path }}</p>
              </td>
              <td class="py-2 text-right tabular-nums text-muted-foreground">{{ fmtNum(c.file_count) }}</td>
              <td class="py-2 text-right tabular-nums">{{ fmtBytes(c.size_bytes) }}</td>
              <td class="py-2 text-right tabular-nums text-muted-foreground">
                {{ pct(c.size_bytes, storage.total_bytes) }}
              </td>
              <td class="py-2 text-right">
                <Button
                  variant="outline"
                  size="sm"
                  :disabled="!c.deletable || cleaningId === c.id"
                  :title="c.deletable ? 'Delete the log files in this category' : 'Nothing to clean'"
                  @click="cleanTarget = c"
                >
                  <Loader2 v-if="cleaningId === c.id" class="h-3.5 w-3.5 animate-spin" />
                  <Trash2 v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
                  Clean
                </Button>
              </td>
            </tr>
          </tbody>
        </table>
        <p class="text-[11px] text-muted-foreground/70 mt-3">
          Dọn log để giải phóng dung lượng. Log của Devdy xóa an toàn — lịch sử run &amp; token vẫn giữ,
          chỉ mất nội dung transcript. Log Claude/Codex là của CLI nên xóa sẽ mất lịch sử phía CLI.
        </p>
      </Card>
    </div>

    <!-- Clean confirm modal -->
    <div
      v-if="cleanTarget"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      @click.self="cleanTarget = null"
    >
      <div class="w-[440px] rounded-lg border border-border bg-card p-5 shadow-xl">
        <div class="flex items-center gap-2 mb-3" :class="cleanTarget.destructive ? 'text-amber-500' : 'text-foreground'">
          <AlertTriangle v-if="cleanTarget.destructive" class="h-5 w-5" />
          <Trash2 v-else class="h-5 w-5" />
          <h2 class="text-sm font-semibold">Clean {{ cleanTarget.label }}</h2>
        </div>
        <p class="text-xs text-muted-foreground leading-relaxed mb-2">{{ cleanTarget.description }}</p>
        <p class="text-xs mb-4">
          Sẽ xóa <strong class="text-foreground">{{ fmtNum(cleanTarget.file_count) }}</strong> file
          (~<strong class="text-foreground">{{ fmtBytes(cleanTarget.size_bytes) }}</strong>).
          <span v-if="cleanTarget.destructive" class="text-amber-500/90">Không thể hoàn tác.</span>
        </p>
        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="cleanTarget = null">Cancel</Button>
          <Button
            variant="destructive"
            :disabled="cleaningId !== null"
            @click="doClean"
          >
            <Loader2 v-if="cleaningId !== null" class="h-3.5 w-3.5 animate-spin" />
            <Trash2 v-else class="h-3.5 w-3.5" />
            Clean now
          </Button>
        </div>
      </div>
    </div>

    <!-- Reset confirm modal -->
    <div
      v-if="showReset"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      @click.self="showReset = false"
    >
      <div class="w-[420px] rounded-lg border border-border bg-card p-5 shadow-xl">
        <div class="flex items-center gap-2 text-red-500 mb-3">
          <AlertTriangle class="h-5 w-5" />
          <h2 class="text-sm font-semibold">Reset usage statistics</h2>
        </div>
        <p class="text-xs text-muted-foreground leading-relaxed mb-1">
          Thao tác này sẽ <strong class="text-foreground">xóa toàn bộ run cũ</strong> (kèm file log)
          và <strong class="text-foreground">đưa mọi số liệu thống kê về 0</strong>.
        </p>
        <p class="text-xs text-red-500/90 mb-4">Không thể hoàn tác.</p>
        <label class="block text-xs text-muted-foreground mb-1.5">Gõ <span class="font-mono text-foreground">RESET</span> để xác nhận:</label>
        <Input
          v-model="resetConfirm"
          type="text"
          size="sm"
          class="mb-4"
          placeholder="RESET"
          @keyup.enter="doReset"
        />
        <div class="flex justify-end gap-2">
          <Button
            variant="outline"
            @click="showReset = false"
          >
            Cancel
          </Button>
          <Button
            variant="destructive"
            :disabled="resetConfirm !== 'RESET' || resetting"
            @click="doReset"
          >
            <Loader2 v-if="resetting" class="h-3.5 w-3.5 animate-spin" />
            <Trash2 v-else class="h-3.5 w-3.5" />
            Reset everything
          </Button>
        </div>
      </div>
    </div>
  </div>
</template>
