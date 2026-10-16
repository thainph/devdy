<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useActivatedRefresh } from '@/composables/useActivatedRefresh'
import {
  CalendarClock, Clock, Activity, DollarSign, ListChecks, Loader2, FolderOpen,
  ChevronLeft, ChevronRight,
} from 'lucide-vue-next'
import { Button, Card, Input, Badge, StatusBadge, AppSelect } from '@/components/ui'
import { useProjectsStore } from '@/stores/projects'
import {
  getWorkDigest,
  type WorkDigestResult,
  type WorkDigestFilter,
  type WorkItem,
} from '@/stores/workDigest'

// Named explicitly: <KeepAlive :include> in App.vue matches on component name,
// and an inferred name is a build detail that minification can change.
defineOptions({ name: 'WorkDigestView' })

const router = useRouter()
const projectsStore = useProjectsStore()
const { t } = useI18n()

// ── date range presets ──────────────────────────────────────────────────────
// `day` = any single local calendar day (date picker + prev/next), defaulting
// to today; the others are multi-day ranges.
const PRESET_OPTIONS = computed(() => [
  { value: 'day', label: t('workDigest.preset.day') },
  { value: '7d', label: t('workDigest.preset.last7d') },
  { value: 'month', label: t('workDigest.preset.thisMonth') },
  { value: 'custom', label: t('workDigest.preset.custom') },
])
const preset = ref('day')

// Local-time YYYY-MM-DD (NOT toISOString, which shifts to UTC).
function fmtLocal(d: Date): string {
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

const customFrom = ref(fmtLocal(new Date()))
const customTo = ref(fmtLocal(new Date()))

// Selected day for the `day` preset (local YYYY-MM-DD).
const day = ref(fmtLocal(new Date()))
const todayYmd = ref(fmtLocal(new Date()))
// Next-day / Today are disabled once the selected day reaches today.
const isToday = computed(() => day.value >= todayYmd.value)

// Shift a local YYYY-MM-DD by `delta` days (parsed as local midnight).
function shiftDay(ymd: string, delta: number): string {
  const [y, m, d] = ymd.split('-').map(Number)
  return fmtLocal(new Date(y, m - 1, d + delta))
}
function prevDay() {
  day.value = shiftDay(day.value || todayYmd.value, -1)
}
function nextDay() {
  refreshToday()
  if (isToday.value) return
  day.value = shiftDay(day.value || todayYmd.value, 1)
}
function goToday() {
  refreshToday()
  day.value = todayYmd.value
}
// The view can stay open (or cached by <KeepAlive>) across midnight. Roll
// `todayYmd` forward, and keep a selection that was "today" following along.
function refreshToday() {
  const now = fmtLocal(new Date())
  if (now === todayYmd.value) return
  if (day.value === todayYmd.value) day.value = now
  todayYmd.value = now
}
const todayTimer = setInterval(refreshToday, 60_000)
onBeforeUnmount(() => clearInterval(todayTimer))
// Clearing the native date input yields '' — fall back to today.
watch(day, (v) => {
  if (!v) day.value = todayYmd.value
})

function presetToRange(p: string): { from: string; to: string } {
  const now = new Date()
  const today = fmtLocal(now)
  if (p === 'day') {
    const d = day.value || today
    return { from: d, to: d }
  }
  if (p === '7d') {
    const start = new Date(now)
    start.setDate(now.getDate() - 6)
    return { from: fmtLocal(start), to: today }
  }
  if (p === 'month') {
    const start = new Date(now.getFullYear(), now.getMonth(), 1)
    return { from: fmtLocal(start), to: today }
  }
  // custom
  return { from: customFrom.value, to: customTo.value }
}

const effectiveRange = computed(() => presetToRange(preset.value))

// AC-02: from > to → validation error, do NOT query.
const rangeError = computed(() => {
  const { from, to } = effectiveRange.value
  if (from && to && from > to) {
    return t('workDigest.rangeError')
  }
  return null
})

// ── project multi-select ────────────────────────────────────────────────────
const selectedProjectIds = ref<Set<string>>(new Set())

function toggleProject(id: string) {
  const next = new Set(selectedProjectIds.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  selectedProjectIds.value = next
}
function selectAll() {
  selectedProjectIds.value = new Set(projectsStore.projects.map((p) => p.id))
}
function selectNone() {
  selectedProjectIds.value = new Set()
}

const noProjectSelected = computed(() => selectedProjectIds.value.size === 0)

// ── data loading ────────────────────────────────────────────────────────────
const digest = ref<WorkDigestResult | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

const currentFilter = computed<WorkDigestFilter>(() => {
  const { from, to } = effectiveRange.value
  return {
    from,
    to,
    project_ids: Array.from(selectedProjectIds.value),
  }
})

async function load() {
  // AC-02: invalid range → skip backend.
  if (rangeError.value) {
    digest.value = null
    return
  }
  // AC-04: nothing selected → empty state, skip backend.
  if (noProjectSelected.value) {
    digest.value = null
    return
  }
  loading.value = true
  error.value = null
  try {
    digest.value = await getWorkDigest(currentFilter.value)
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
}

watch(currentFilter, load, { deep: true })

onMounted(async () => {
  if (projectsStore.projects.length === 0) await projectsStore.fetchProjects()
  // Default: select all projects.
  selectAll()
  await load()
})

// Cached by <KeepAlive> (App.vue). Only `load()` belongs here — NOT `selectAll()`:
// that is a first-run default, and re-applying it on every visit would silently
// throw away the project filter the user picked last time (which <KeepAlive> is
// now preserving for them).
useActivatedRefresh(() => {
  refreshToday()
  load()
})

// ── formatting ──────────────────────────────────────────────────────────────
function fmtDuration(secs: number | null): string {
  if (secs === null || secs === undefined) return '—'
  if (secs <= 0) return '—'
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  const s = secs % 60
  if (h > 0) return `${h}h ${m}m`
  if (m > 0) return `${m}m ${s}s`
  return `${s}s`
}

function fmtCost(n: number): string {
  if (!n) return '$0'
  if (n < 0.01) return '$' + n.toFixed(4)
  return '$' + n.toFixed(2)
}

function fmtNum(n: number): string {
  return n.toLocaleString('en-US')
}

function fmtTime(iso: string | null): string {
  if (!iso) return '—'
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return '—'
  return d.toLocaleString('en-US', {
    day: '2-digit', month: '2-digit',
    hour: '2-digit', minute: '2-digit',
  })
}

const TYPE_LABEL: Record<string, string> = {
  session: 'workDigest.typeSession',
  analyze_issue: 'workDigest.typeIssue',
  review_pr: 'workDigest.typePr',
}
function typeLabel(type: string): string {
  const key = TYPE_LABEL[type]
  return key ? t(key) : type
}

const hasData = computed(() => (digest.value?.summary.total_items ?? 0) > 0)

function openItem(item: WorkItem) {
  router.push({
    name: 'project-run-detail',
    params: { projectId: item.project_id, runId: item.id },
  })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <h1 class="text-sm font-semibold">{{ t('workDigest.title') }}</h1>
      <div class="flex items-center gap-3">
        <span v-if="loading" class="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Loader2 class="h-3.5 w-3.5 animate-spin" /> {{ t('workDigest.loading') }}
        </span>
      </div>
    </div>

    <!-- Body -->
    <div class="flex-1 overflow-auto p-6 space-y-5">
      <!-- Filters -->
      <div class="flex flex-wrap items-center gap-2">
        <AppSelect v-model="preset" :options="PRESET_OPTIONS" size="sm" class="w-40" />
        <template v-if="preset === 'day'">
          <Button variant="ghost" size="icon-sm" :title="t('workDigest.prevDay')" @click="prevDay">
            <ChevronLeft class="h-4 w-4" />
          </Button>
          <Input v-model="day" type="date" size="sm" class="w-40" :max="todayYmd" />
          <Button
            variant="ghost"
            size="icon-sm"
            :title="t('workDigest.nextDay')"
            :disabled="isToday"
            @click="nextDay"
          >
            <ChevronRight class="h-4 w-4" />
          </Button>
          <Button variant="outline" size="sm" :disabled="isToday" @click="goToday">
            {{ t('workDigest.preset.today') }}
          </Button>
        </template>
        <template v-else-if="preset === 'custom'">
          <Input v-model="customFrom" type="date" size="sm" class="w-40" />
          <span class="text-xs text-muted-foreground">→</span>
          <Input v-model="customTo" type="date" size="sm" class="w-40" />
        </template>
      </div>

      <p v-if="rangeError" class="text-xs text-red-500">{{ rangeError }}</p>

      <!-- Project multi-select -->
      <Card class="border-border/60" body-class="p-4">
        <div class="flex items-center justify-between mb-3">
          <h2 class="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('workDigest.projects') }}
          </h2>
          <div class="flex items-center gap-2">
            <Button variant="ghost" size="sm" @click="selectAll">{{ t('workDigest.selectAll') }}</Button>
            <Button variant="ghost" size="sm" @click="selectNone">{{ t('common.clear') }}</Button>
          </div>
        </div>
        <div v-if="projectsStore.projects.length === 0" class="text-xs text-muted-foreground">
          {{ t('workDigest.noProjects') }}
        </div>
        <div v-else class="flex flex-wrap gap-2">
          <label
            v-for="p in projectsStore.projects"
            :key="p.id"
            class="flex items-center gap-1.5 text-xs rounded border border-border/60 px-2 py-1 cursor-pointer hover:bg-muted/50"
            :class="selectedProjectIds.has(p.id) ? 'bg-muted/60 text-foreground' : 'text-muted-foreground'"
          >
            <input
              type="checkbox"
              class="accent-primary"
              :checked="selectedProjectIds.has(p.id)"
              @change="toggleProject(p.id)"
            />
            {{ p.name }}
          </label>
        </div>
      </Card>

      <p v-if="error" class="text-xs text-red-500">{{ error }}</p>

      <!-- AC-04: empty state — no project selected -->
      <div
        v-if="noProjectSelected && !rangeError"
        class="flex flex-col items-center justify-center py-16 text-center"
      >
        <FolderOpen class="h-8 w-8 text-muted-foreground/40 mb-3" :stroke-width="1.5" />
        <p class="text-sm text-muted-foreground">
          {{ t('workDigest.emptyNoProject') }}
        </p>
      </div>

      <!-- Empty result -->
      <div
        v-else-if="!loading && !rangeError && !hasData"
        class="flex flex-col items-center justify-center py-16 text-center"
      >
        <CalendarClock class="h-8 w-8 text-muted-foreground/40 mb-3" :stroke-width="1.5" />
        <p class="text-sm text-muted-foreground">{{ t('workDigest.emptyNoWork') }}</p>
      </div>

      <template v-if="hasData && digest && !rangeError && !noProjectSelected">
        <!-- Summary cards -->
        <div class="grid grid-cols-2 lg:grid-cols-4 gap-3">
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <ListChecks class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('workDigest.workItems') }}
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtNum(digest.summary.total_items) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Clock class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('workDigest.totalDuration') }}
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtDuration(digest.summary.total_wall_secs) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Activity class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('workDigest.activeTime') }}
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtDuration(digest.summary.total_active_secs) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <DollarSign class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('workDigest.totalCost') }}
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtCost(digest.summary.total_cost) }}</p>
          </Card>
        </div>

        <!-- Project groups -->
        <Card
          v-for="g in digest.projects"
          :key="g.project_id ?? 'none'"
          class="border-border/60"
          body-class="p-0"
        >
          <template #header>
            <div class="flex items-center justify-between w-full">
              <h2 class="text-xs font-semibold">{{ g.project_name ?? t('workDigest.deletedProject') }}</h2>
              <div class="flex items-center gap-1.5">
                <Badge tone="neutral">{{ t('workDigest.items', { n: g.item_count }) }}</Badge>
                <Badge tone="primary">{{ fmtDuration(g.wall_secs) }}</Badge>
                <Badge tone="info">{{ t('workDigest.activeSuffix', { duration: fmtDuration(g.active_secs) }) }}</Badge>
                <Badge tone="neutral">{{ t('workDigest.tokensSuffix', { n: fmtNum(g.tokens) }) }}</Badge>
                <Badge tone="neutral">{{ fmtCost(g.cost) }}</Badge>
              </div>
            </div>
          </template>

          <table class="w-full text-xs">
            <thead>
              <tr class="text-muted-foreground/70 text-left">
                <th class="font-normal px-4 py-2">{{ t('workDigest.colWork') }}</th>
                <th class="font-normal px-2 py-2">{{ t('workDigest.colType') }}</th>
                <th class="font-normal px-2 py-2">{{ t('workDigest.colEngine') }}</th>
                <th class="font-normal px-2 py-2">{{ t('workDigest.colStatus') }}</th>
                <th class="font-normal px-2 py-2">{{ t('workDigest.colStarted') }}</th>
                <th class="font-normal px-2 py-2 text-right">{{ t('workDigest.colDuration') }}</th>
                <th class="font-normal px-2 py-2 text-right">{{ t('workDigest.colActive') }}</th>
                <th class="font-normal px-2 py-2 text-right">{{ t('workDigest.colTokens') }}</th>
                <th class="font-normal px-4 py-2 text-right">{{ t('workDigest.colCost') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="item in g.items"
                :key="item.id"
                class="border-t border-border/40 cursor-pointer hover:bg-muted/40"
                @click="openItem(item)"
              >
                <td class="px-4 py-2 max-w-[280px] truncate">{{ item.description }}</td>
                <td class="px-2 py-2">
                  <Badge tone="neutral">{{ typeLabel(item.run_type) }}</Badge>
                </td>
                <td class="px-2 py-2">
                  <Badge tone="neutral">{{ item.engine }}</Badge>
                </td>
                <td class="px-2 py-2">
                  <StatusBadge :status="item.status" size="xs" />
                </td>
                <td class="px-2 py-2 text-muted-foreground whitespace-nowrap">{{ fmtTime(item.started_at) }}</td>
                <td class="px-2 py-2 text-right tabular-nums whitespace-nowrap">{{ fmtDuration(item.wall_secs) }}</td>
                <td class="px-2 py-2 text-right tabular-nums whitespace-nowrap">{{ fmtDuration(item.active_secs) }}</td>
                <td class="px-2 py-2 text-right tabular-nums text-muted-foreground">{{ fmtNum(item.tokens) }}</td>
                <td class="px-4 py-2 text-right tabular-nums text-muted-foreground">{{ fmtCost(item.cost) }}</td>
              </tr>
            </tbody>
          </table>
        </Card>
      </template>
    </div>
  </div>
</template>
