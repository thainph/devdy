<script setup lang="ts">
// Usage Stats: tokens / cost / runs recorded per run, filtered by range,
// engine, project and Claude account (all in the page header), plus a
// read-only disk-usage strip. Laid out dense: one KPI strip, the daily chart
// beside the engine split, three ranked breakdowns side by side, storage last.
import { ref, computed, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  Chart as ChartJS,
  Tooltip,
  BarController,
  BarElement,
  CategoryScale,
  LinearScale,
} from 'chart.js'
import { Bar } from 'vue-chartjs'
import { Coins, RefreshCw, Trash2, Loader2, Info, AlertTriangle } from 'lucide-vue-next'
import { Button, Input, Card, AppSelect, PageHeader, PageHeaderButton, SegmentedControl, type SegmentedOption } from '@/components/ui'
import RankedTable, { type RankedRow } from '@/components/stats/RankedTable.vue'
import { useProjectsStore } from '@/stores/projects'
import { useClaudeAccountsStore } from '@/stores/claudeAccounts'
import {
  getUsageStats, backfillUsage, resetUsageStats,
  getStorageStats, NO_ACCOUNT,
  type StatsResult, type StatsFilter, type StorageStats, type AccountStat,
} from '@/stores/stats'

// Register explicitly: vue-chartjs's own register calls are marked pure and get
// tree-shaken out of production builds.
ChartJS.register(Tooltip, BarController, BarElement, CategoryScale, LinearScale)

const { t } = useI18n()
const projectsStore = useProjectsStore()
const accountsStore = useClaudeAccountsStore()

// ── filters ────────────────────────────────────────────────────────────────
type Range = '7' | '30' | '90' | 'all'
const RANGE_OPTIONS = computed<SegmentedOption<Range>[]>(() => [
  { value: '7', label: t('stats.range.short7'), title: t('stats.range.last7') },
  { value: '30', label: t('stats.range.short30'), title: t('stats.range.last30') },
  { value: '90', label: t('stats.range.short90'), title: t('stats.range.last90') },
  { value: 'all', label: t('stats.range.shortAll'), title: t('stats.range.all') },
])
const range = ref<Range>('30')
const engine = ref('') // '' = all
const projectId = ref('') // '' = all
const accountId = ref('') // '' = all; NO_ACCOUNT = global ~/.claude

const engineOptions = computed(() => [
  { value: '', label: t('stats.engine.all') },
  { value: 'claude', label: 'Claude' },
  { value: 'codex', label: 'Codex' },
])
const projectOptions = computed(() => [
  { value: '', label: t('stats.project.all') },
  ...projectsStore.projects.map((p) => ({ value: p.id, label: p.name })),
])

// Only meaningful once managed Claude accounts exist; picking one implies Claude.
const hasAccounts = computed(() => accountsStore.accounts.length > 0)
const accountOptions = computed(() => [
  { value: '', label: t('stats.account.all') },
  ...accountsStore.accounts.map((a) => ({ value: a.id, label: a.label })),
  { value: NO_ACCOUNT, label: t('stats.account.none') },
])
function accountName(a: AccountStat): string {
  if (!a.account_id) return t('stats.account.none')
  return a.account_label ?? t('stats.account.deleted')
}

function rangeToDates(r: Range): { from: string | null; to: string | null } {
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
    account_id: accountId.value || null,
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
  if (accountsStore.accounts.length === 0) await accountsStore.fetch()
  await Promise.all([load(), loadStorage()])
})

// ── formatting ────────────────────────────────────────────────────────────────
function fmtNum(n: number): string {
  return n.toLocaleString('en-US')
}
function fmtCompact(n: number): string {
  if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(2) + 'B'
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M'
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k'
  return String(Math.round(n))
}
function fmtCost(n: number): string {
  if (n === 0) return '$0'
  if (n < 0.01) return '$' + n.toFixed(4)
  return '$' + n.toFixed(2)
}
function pct(part: number, total: number): string {
  if (!total) return '0%'
  return ((part / total) * 100).toFixed(0) + '%'
}

const hasData = computed(() => (stats.value?.summary.total_turns ?? 0) > 0)

// Series colours (data palette, not UI state — raw hex is fine here).
const PALETTE = ['#6366f1', '#22c55e', '#f59e0b', '#ec4899', '#06b6d4', '#a855f7', '#ef4444', '#14b8a6']

// ── KPI strip ──────────────────────────────────────────────────────────────────
const tokenParts = computed(() => {
  const s = stats.value?.summary
  if (!s) return []
  return [
    { key: 'input', label: t('stats.summary.input'), value: s.total_input, color: PALETTE[0] },
    { key: 'output', label: t('stats.summary.output'), value: s.total_output, color: PALETTE[1] },
    { key: 'cache', label: t('stats.summary.cache'), value: s.total_cache, color: PALETTE[2] },
  ]
})
/** Days the average cost is spread over: the range, or the days with data for "All". */
const dayCount = computed(() => (range.value === 'all' ? Math.max(1, stats.value?.daily.length ?? 1) : parseInt(range.value, 10)))

// ── daily chart: one metric at a time ─────────────────────────────────────────
type Metric = 'tokens' | 'cost' | 'runs'
const metric = ref<Metric>('tokens')
const METRIC_OPTIONS = computed<SegmentedOption<Metric>[]>(() => [
  { value: 'tokens', label: t('stats.charts.tokens') },
  { value: 'cost', label: t('stats.charts.cost') },
  { value: 'runs', label: t('stats.charts.runs') },
])
const METRIC_COLOR: Record<Metric, string> = { tokens: PALETTE[0], cost: PALETTE[2], runs: PALETTE[1] }
const fmtMetric = (m: Metric, v: number) => (m === 'cost' ? fmtCost(v) : m === 'runs' ? fmtNum(v) : fmtCompact(v))

const gridColor = 'rgba(127,127,127,0.12)'
const tickColor = 'rgba(148,148,160,0.9)'

const dailyChartData = computed(() => {
  const d = stats.value?.daily ?? []
  return {
    labels: d.map((p) => p.date.slice(5)), // MM-DD
    datasets: [{
      label: METRIC_OPTIONS.value.find((o) => o.value === metric.value)?.label ?? '',
      data: d.map((p) => p[metric.value]),
      backgroundColor: METRIC_COLOR[metric.value] + 'b3',
      hoverBackgroundColor: METRIC_COLOR[metric.value],
      borderRadius: 2,
      maxBarThickness: 28,
    }],
  }
})

const dailyChartOptions = computed(() => {
  const m = metric.value
  return {
    responsive: true,
    maintainAspectRatio: false,
    interaction: { mode: 'index' as const, intersect: false },
    plugins: {
      legend: { display: false },
      tooltip: { callbacks: { label: (ctx: { parsed: { y: number | null } }) => fmtMetric(m, ctx.parsed.y ?? 0) } },
    },
    scales: {
      x: { grid: { display: false }, ticks: { color: tickColor, font: { size: 10 }, maxRotation: 0, autoSkip: true, autoSkipPadding: 12 } },
      y: {
        grid: { color: gridColor },
        border: { display: false },
        ticks: { color: tickColor, font: { size: 10 }, maxTicksLimit: 5, callback: (v: number | string) => fmtMetric(m, Number(v)) },
      },
    },
  }
})

// ── breakdowns ────────────────────────────────────────────────────────────────
const projectRows = computed<RankedRow[]>(() => (stats.value?.by_project ?? []).map((p) => ({
  key: p.project_id ?? 'none', name: p.project_name ?? t('stats.table.deletedProject'), tokens: p.tokens, cost: p.cost,
})))
const modelRows = computed<RankedRow[]>(() => (stats.value?.by_model ?? []).map((m) => ({
  key: m.model ?? 'unknown', name: m.model ?? t('stats.table.unknownModel'), tokens: m.tokens, cost: m.cost,
})))
const accountRows = computed<RankedRow[]>(() => (stats.value?.by_account ?? []).map((a) => ({
  key: a.account_id ?? 'none', name: accountName(a), tokens: a.tokens, cost: a.cost,
})))
const showAccounts = computed(() => hasAccounts.value && accountRows.value.length > 0)

// ── actions: backfill + reset ──────────────────────────────────────────────────
const backfilling = ref(false)
const actionMsg = ref<string | null>(null)

async function doBackfill() {
  backfilling.value = true
  actionMsg.value = null
  try {
    const r = await backfillUsage()
    actionMsg.value = t('stats.action.backfilled', { inserted: r.inserted, scanned: r.runs_scanned })
    await load()
  } catch (e) {
    actionMsg.value = t('stats.action.backfillFailed', { error: String(e) })
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
    actionMsg.value = t('stats.action.resetDone', { runs: r.runs_deleted, usage: r.usage_cleared })
    showReset.value = false
    await load()
  } catch (e) {
    actionMsg.value = t('stats.action.resetFailed', { error: String(e) })
  } finally {
    resetting.value = false
  }
}

// ── storage: disk usage (read-only; space is freed by deleting sessions) ───────
const storage = ref<StorageStats | null>(null)
const storageLoading = ref(false)

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
    actionMsg.value = t('stats.action.storageReadFailed', { error: String(e) })
  } finally {
    storageLoading.value = false
  }
}
</script>

<template>
  <div class="@container/screen flex flex-col h-full">
    <!-- Header: title, filters, actions -->
    <PageHeader :title="t('stats.title')">
      <!-- Filters: in the header when it is wide, else in a strip below it. -->
      <template #meta>
        <div class="ml-2 hidden min-w-0 items-center gap-2 overflow-x-auto [scrollbar-width:none] @[64rem]/page-header:flex">
          <SegmentedControl v-model="range" :options="RANGE_OPTIONS" :stretch="false" />
          <AppSelect v-model="engine" :options="engineOptions" size="sm" class="w-32 shrink-0" />
          <AppSelect v-model="projectId" :options="projectOptions" size="sm" class="w-40 shrink-0" />
          <AppSelect v-if="hasAccounts" v-model="accountId" :options="accountOptions" size="sm" class="w-44 shrink-0" />
          <Loader2 v-if="loading" class="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" :aria-label="t('common.loading')" />
        </div>
      </template>
      <template #actions>
        <PageHeaderButton
          :label="t('stats.backfill')"
          :disabled="backfilling"
          :title="t('stats.backfillTitle')"
          @click="doBackfill"
        >
          <Loader2 v-if="backfilling" class="h-3.5 w-3.5 animate-spin" />
          <RefreshCw v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        </PageHeaderButton>
        <PageHeaderButton
          variant="destructive-ghost"
          :label="t('stats.reset')"
          :title="t('stats.resetTitle')"
          @click="openReset"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        </PageHeaderButton>
      </template>
    </PageHeader>
    <div class="flex shrink-0 flex-wrap items-center gap-2 border-b border-border/60 px-3 py-2 @[40rem]/screen:px-6 @[64rem]/screen:hidden">
      <SegmentedControl v-model="range" :options="RANGE_OPTIONS" :stretch="false" />
      <AppSelect v-model="engine" :options="engineOptions" size="sm" class="w-32 min-w-0" />
      <AppSelect v-model="projectId" :options="projectOptions" size="sm" class="w-40 min-w-0" />
      <AppSelect v-if="hasAccounts" v-model="accountId" :options="accountOptions" size="sm" class="w-44 min-w-0" />
      <Loader2 v-if="loading" class="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" :aria-label="t('common.loading')" />
    </div>

    <!-- Body -->
    <div class="flex-1 overflow-auto px-3 py-4 space-y-3 @[40rem]/screen:px-6">
      <p v-if="actionMsg" class="text-xs text-muted-foreground">{{ actionMsg }}</p>
      <p v-if="error" class="text-xs text-destructive">{{ error }}</p>

      <!-- Empty state -->
      <div
        v-if="!loading && !hasData"
        class="flex flex-col items-center justify-center py-16 text-center"
      >
        <Coins class="h-8 w-8 text-muted-foreground/40 mb-3" :stroke-width="1.5" />
        <p class="text-sm text-muted-foreground">{{ t('stats.empty.title') }}</p>
        <p class="text-xs text-muted-foreground/70 mt-1">
          {{ t('stats.empty.hint') }}
        </p>
      </div>

      <template v-if="hasData && stats">
        <!-- KPI strip -->
        <Card class="border-border/60" body-class="grid grid-cols-2 @[52rem]/screen:grid-cols-[1.6fr_1fr_1fr_1fr] divide-border/50 @[52rem]/screen:divide-x [&>*:nth-child(n+3)]:border-t [&>*:nth-child(n+3)]:border-border/50 @[52rem]/screen:[&>*:nth-child(n+3)]:border-t-0">
          <div class="flex min-w-0 flex-col gap-1 px-4 py-2.5">
            <span class="text-[11px] font-semibold text-muted-foreground">{{ t('stats.summary.totalTokens') }}</span>
            <span class="text-xl font-extrabold leading-tight tabular-nums" :title="fmtNum(stats.summary.total_tokens)">{{ fmtCompact(stats.summary.total_tokens) }}</span>
            <div class="flex h-1.5 gap-0.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
              <template v-for="p in tokenParts" :key="p.key">
                <span v-if="p.value > 0" :style="{ flex: p.value, background: p.color }" />
              </template>
            </div>
            <div class="flex flex-wrap gap-x-3 text-[11px] text-muted-foreground">
              <span v-for="p in tokenParts" :key="p.key" class="inline-flex items-center gap-1">
                <span class="h-1.5 w-1.5 rounded-sm" :style="{ background: p.color }" />
                {{ p.label }} <span class="tabular-nums">{{ fmtCompact(p.value) }}</span>
              </span>
            </div>
          </div>
          <div class="flex min-w-0 flex-col gap-1 px-4 py-2.5">
            <span class="flex items-center gap-1 text-[11px] font-semibold text-muted-foreground">
              {{ t('stats.summary.estCost') }}
              <span :title="t('stats.summary.estCostTooltip')"><Info class="h-3 w-3 text-muted-foreground/60" /></span>
            </span>
            <span class="text-xl font-extrabold leading-tight tabular-nums">{{ fmtCost(stats.summary.total_cost) }}</span>
            <span class="text-[11px] text-muted-foreground tabular-nums">{{ t('stats.summary.perDay', { cost: fmtCost(stats.summary.total_cost / dayCount) }) }}</span>
          </div>
          <div class="flex min-w-0 flex-col gap-1 px-4 py-2.5">
            <span class="text-[11px] font-semibold text-muted-foreground">{{ t('stats.summary.runs') }}</span>
            <span class="text-xl font-extrabold leading-tight tabular-nums">{{ fmtNum(stats.summary.total_runs) }}</span>
            <span v-if="stats.summary.total_runs" class="text-[11px] text-muted-foreground tabular-nums">
              {{ t('stats.summary.tokensPerRun', { n: fmtCompact(stats.summary.total_tokens / stats.summary.total_runs) }) }}
            </span>
          </div>
          <div class="flex min-w-0 flex-col gap-1 px-4 py-2.5">
            <span class="text-[11px] font-semibold text-muted-foreground">{{ t('stats.summary.turns') }}</span>
            <span class="text-xl font-extrabold leading-tight tabular-nums">{{ fmtNum(stats.summary.total_turns) }}</span>
            <span v-if="stats.summary.total_runs" class="text-[11px] text-muted-foreground tabular-nums">
              {{ t('stats.summary.turnsPerRun', { n: (stats.summary.total_turns / stats.summary.total_runs).toFixed(1) }) }}
            </span>
          </div>
        </Card>

        <!-- Daily chart + engine split -->
        <div class="grid grid-cols-1 gap-3 @[52rem]/screen:grid-cols-[minmax(0,2.4fr)_minmax(0,1fr)]">
          <Card class="border-border/60 min-w-0" body-class="px-3 pt-2.5 pb-3">
            <div class="mb-2 flex items-center gap-2">
              <h2 class="text-[11px] font-semibold text-muted-foreground">{{ t('stats.charts.daily') }}</h2>
              <SegmentedControl v-model="metric" :options="METRIC_OPTIONS" :stretch="false" class="ml-auto" />
            </div>
            <div class="h-48">
              <Bar :data="dailyChartData" :options="dailyChartOptions" />
            </div>
          </Card>

          <Card class="border-border/60 min-w-0" body-class="flex flex-col gap-2.5 px-3 pt-2.5 pb-3">
            <h2 class="text-[11px] font-semibold text-muted-foreground">{{ t('stats.charts.byEngine') }}</h2>
            <div class="flex h-2.5 gap-0.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
              <template v-for="(e, i) in stats.by_engine" :key="e.engine">
                <span v-if="e.tokens > 0" :style="{ flex: e.tokens, background: PALETTE[i % PALETTE.length] }" />
              </template>
            </div>
            <div v-for="(e, i) in stats.by_engine" :key="e.engine" class="min-w-0">
              <div class="flex items-baseline justify-between gap-2">
                <span class="flex items-center gap-1.5 text-[13px] font-semibold capitalize">
                  <span class="h-2 w-2 rounded-sm" :style="{ background: PALETTE[i % PALETTE.length] }" />
                  {{ e.engine }}
                </span>
                <span class="text-lg font-extrabold tabular-nums">{{ pct(e.tokens, stats.summary.total_tokens) }}</span>
              </div>
              <p class="pl-3.5 text-[11px] text-muted-foreground tabular-nums">
                {{ t('stats.engineLine', { tokens: fmtCompact(e.tokens), cost: fmtCost(e.cost), runs: fmtNum(e.runs) }) }}
              </p>
            </div>
          </Card>
        </div>

        <!-- Breakdowns -->
        <div class="grid grid-cols-1 gap-3" :class="showAccounts ? '@[52rem]/screen:grid-cols-3' : '@[52rem]/screen:grid-cols-2'">
          <RankedTable :title="t('stats.charts.byProject')" :rows="projectRows" :total="stats.summary.total_tokens" />
          <RankedTable :title="t('stats.table.byModel')" :rows="modelRows" :total="stats.summary.total_tokens" mono />
          <RankedTable v-if="showAccounts" :title="t('stats.table.byAccount')" :rows="accountRows" :total="stats.summary.total_tokens" />
        </div>
      </template>

      <!-- Storage: disk usage (read-only, independent of token data) -->
      <Card class="border-border/60" body-class="px-3 pt-2 pb-2.5">
        <div class="flex items-center gap-2">
          <h2 class="text-[11px] font-semibold text-muted-foreground">
            {{ t('stats.storage.title') }}
            <span v-if="storage" class="font-normal">{{ t('stats.storage.totalSuffix', { bytes: fmtBytes(storage.total_bytes) }) }}</span>
          </h2>
          <Button variant="ghost" size="xs" class="ml-auto" :disabled="storageLoading" @click="loadStorage">
            <Loader2 v-if="storageLoading" class="h-3 w-3 animate-spin" />
            <RefreshCw v-else class="h-3 w-3" :stroke-width="2" />
            {{ t('stats.storage.rescan') }}
          </Button>
        </div>

        <p v-if="!storage && storageLoading" class="py-3 text-xs text-muted-foreground">{{ t('stats.storage.scanning') }}</p>

        <template v-else-if="storage">
          <div class="mt-1.5 flex h-1.5 gap-0.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
            <template v-for="(c, i) in storage.categories" :key="c.id">
              <span v-if="c.size_bytes > 0" :style="{ flex: c.size_bytes, background: PALETTE[i % PALETTE.length] }" />
            </template>
          </div>
          <div class="mt-2 grid grid-cols-1 gap-3 @[36rem]/screen:grid-cols-3">
            <div v-for="(c, i) in storage.categories" :key="c.id" class="min-w-0" :title="c.description">
              <p class="flex items-center gap-1.5 truncate text-[11px] font-medium">
                <span class="h-1.5 w-1.5 shrink-0 rounded-sm" :style="{ background: PALETTE[i % PALETTE.length] }" />
                {{ c.label }}
              </p>
              <p class="tabular-nums">
                <span class="text-sm font-bold">{{ fmtBytes(c.size_bytes) }}</span>
                <span class="text-[11px] text-muted-foreground">
                  · {{ pct(c.size_bytes, storage.total_bytes) }} · {{ t('stats.storage.fileCount', { n: fmtNum(c.file_count) }) }}
                </span>
              </p>
              <p class="truncate font-mono text-[10px] text-muted-foreground">{{ c.path }}</p>
              <p v-if="c.parts.length" class="mt-0.5 truncate text-[11px] text-muted-foreground tabular-nums" :title="c.parts.map((p) => `${p.name} ${fmtBytes(p.size_bytes)}`).join(' · ')">
                <template v-for="(p, j) in c.parts" :key="p.name">{{ j ? ' · ' : '' }}{{ p.name }} {{ fmtBytes(p.size_bytes) }}</template>
              </p>
            </div>
          </div>
          <p class="mt-2 text-[11px] text-muted-foreground/80">{{ t('stats.storage.note') }}</p>
        </template>
      </Card>
    </div>

    <!-- Reset confirm modal -->
    <div
      v-if="showReset"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      @click.self="showReset = false"
    >
      <div class="w-[420px] rounded-lg border border-border bg-card p-5 shadow-xl">
        <div class="flex items-center gap-2 text-destructive mb-3">
          <AlertTriangle class="h-5 w-5" />
          <h2 class="text-sm font-semibold">{{ t('stats.resetModal.title') }}</h2>
        </div>
        <p class="text-xs text-muted-foreground leading-relaxed mb-1">
          {{ t('stats.resetModal.line1') }}
        </p>
        <p class="text-xs text-destructive/90 mb-4">{{ t('stats.resetModal.irreversible') }}</p>
        <label class="block text-xs text-muted-foreground mb-1.5">{{ t('stats.resetModal.typeToConfirm', { word: 'RESET' }) }}</label>
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
            variant="ghost"
            @click="showReset = false"
          >
            {{ t('common.cancel') }}
          </Button>
          <Button
            variant="destructive"
            :disabled="resetConfirm !== 'RESET' || resetting"
            @click="doReset"
          >
            <Loader2 v-if="resetting" class="h-3.5 w-3.5 animate-spin" />
            <Trash2 v-else class="h-3.5 w-3.5" />
            {{ t('stats.resetModal.resetEverything') }}
          </Button>
        </div>
      </div>
    </div>
  </div>
</template>
