<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  CalendarClock, Clock, Activity, DollarSign, ListChecks, Loader2, FolderOpen,
} from 'lucide-vue-next'
import { Button, Card, Input, Badge, StatusBadge, AppSelect } from '@/components/ui'
import { useProjectsStore } from '@/stores/projects'
import {
  getWorkDigest,
  type WorkDigestResult,
  type WorkDigestFilter,
  type WorkItem,
} from '@/stores/workDigest'

const router = useRouter()
const projectsStore = useProjectsStore()

// ── date range presets ──────────────────────────────────────────────────────
const PRESET_OPTIONS = [
  { value: 'today', label: 'Today' },
  { value: 'yesterday', label: 'Yesterday' },
  { value: '7d', label: 'Last 7 days' },
  { value: 'month', label: 'This month' },
  { value: 'custom', label: 'Custom' },
]
const preset = ref('today')

// Local-time YYYY-MM-DD (NOT toISOString, which shifts to UTC).
function fmtLocal(d: Date): string {
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

const customFrom = ref(fmtLocal(new Date()))
const customTo = ref(fmtLocal(new Date()))

function presetToRange(p: string): { from: string; to: string } {
  const now = new Date()
  const today = fmtLocal(now)
  if (p === 'today') return { from: today, to: today }
  if (p === 'yesterday') {
    const y = new Date(now)
    y.setDate(now.getDate() - 1)
    const yd = fmtLocal(y)
    return { from: yd, to: yd }
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
    return 'Start date must be on or before the end date.'
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
  session: 'Session',
  analyze_issue: 'Issue',
  review_pr: 'PR',
}
function typeLabel(t: string): string {
  return TYPE_LABEL[t] ?? t
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
      <h1 class="text-sm font-semibold">Work Digest</h1>
      <span v-if="loading" class="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Loader2 class="h-3.5 w-3.5 animate-spin" /> Loading…
      </span>
    </div>

    <!-- Body -->
    <div class="flex-1 overflow-auto p-6 space-y-5">
      <!-- Filters -->
      <div class="flex flex-wrap items-center gap-2">
        <AppSelect v-model="preset" :options="PRESET_OPTIONS" size="sm" class="w-40" />
        <template v-if="preset === 'custom'">
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
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" /> Projects
          </h2>
          <div class="flex items-center gap-2">
            <Button variant="ghost" size="sm" @click="selectAll">Select all</Button>
            <Button variant="ghost" size="sm" @click="selectNone">Clear</Button>
          </div>
        </div>
        <div v-if="projectsStore.projects.length === 0" class="text-xs text-muted-foreground">
          No projects yet.
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
          No work to show — select at least one project.
        </p>
      </div>

      <!-- Empty result -->
      <div
        v-else-if="!loading && !rangeError && !hasData"
        class="flex flex-col items-center justify-center py-16 text-center"
      >
        <CalendarClock class="h-8 w-8 text-muted-foreground/40 mb-3" :stroke-width="1.5" />
        <p class="text-sm text-muted-foreground">No work recorded in this time range.</p>
      </div>

      <template v-if="hasData && digest && !rangeError && !noProjectSelected">
        <!-- Summary cards -->
        <div class="grid grid-cols-2 lg:grid-cols-4 gap-3">
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <ListChecks class="h-3.5 w-3.5" :stroke-width="1.75" /> Work items
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtNum(digest.summary.total_items) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Clock class="h-3.5 w-3.5" :stroke-width="1.75" /> Total duration
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtDuration(digest.summary.total_wall_secs) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <Activity class="h-3.5 w-3.5" :stroke-width="1.75" /> Active time
            </div>
            <p class="text-xl font-semibold tabular-nums">{{ fmtDuration(digest.summary.total_active_secs) }}</p>
          </Card>
          <Card class="border-border/60" body-class="p-4">
            <div class="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
              <DollarSign class="h-3.5 w-3.5" :stroke-width="1.75" /> Total cost
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
              <h2 class="text-xs font-semibold">{{ g.project_name ?? '(deleted)' }}</h2>
              <div class="flex items-center gap-1.5">
                <Badge tone="neutral">{{ g.item_count }} items</Badge>
                <Badge tone="primary">{{ fmtDuration(g.wall_secs) }}</Badge>
                <Badge tone="info">{{ fmtDuration(g.active_secs) }} active</Badge>
                <Badge tone="neutral">{{ fmtNum(g.tokens) }} tokens</Badge>
                <Badge tone="neutral">{{ fmtCost(g.cost) }}</Badge>
              </div>
            </div>
          </template>

          <table class="w-full text-xs">
            <thead>
              <tr class="text-muted-foreground/70 text-left">
                <th class="font-normal px-4 py-2">Work</th>
                <th class="font-normal px-2 py-2">Type</th>
                <th class="font-normal px-2 py-2">Engine</th>
                <th class="font-normal px-2 py-2">Status</th>
                <th class="font-normal px-2 py-2">Started</th>
                <th class="font-normal px-2 py-2 text-right">Duration</th>
                <th class="font-normal px-2 py-2 text-right">Active</th>
                <th class="font-normal px-2 py-2 text-right">Tokens</th>
                <th class="font-normal px-4 py-2 text-right">Cost</th>
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
