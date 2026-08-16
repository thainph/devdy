<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import {
  useProjectIssuesStore,
  type MilestoneGroup,
} from '@/stores/projectIssues'
import { Button, Card } from '@/components/ui'
import BackToRunButton from '@/components/BackToRunButton.vue'
import GanttChart from '@/components/GanttChart.vue'
import { RefreshCw, CircleDot, AlertTriangle, Info } from 'lucide-vue-next'

const route = useRoute()
const store = useProjectIssuesStore()
const projectId = computed(() => route.params.projectId as string)

const DAY = 86_400_000
const selectedRepos = ref<Set<string>>(new Set())

onMounted(() => {
  if (store.currentProjectId !== projectId.value || !store.board) {
    store.refresh(projectId.value)
  }
})

// ── Repo filter ────────────────────────────────────────────────────────────
const allRepos = computed(() => {
  const set = new Set<string>()
  for (const g of store.board?.milestones ?? []) for (const r of g.repos) set.add(r)
  return [...set].sort()
})
function toggleRepo(repo: string) {
  const next = new Set(selectedRepos.value)
  if (next.has(repo)) next.delete(repo)
  else next.add(repo)
  selectedRepos.value = next
}
function repoActive(repo: string) {
  return selectedRepos.value.size === 0 || selectedRepos.value.has(repo)
}

// ── Milestone sorting (overdue → at-risk → on-track → none) ───────────────────
type MsStatus = 'overdue' | 'at-risk' | 'ontrack' | 'none'
function msStatus(g: MilestoneGroup): MsStatus {
  if (g.title === 'No milestone') return 'none'
  const due = g.dueOn ? new Date(g.dueOn).getTime() : null
  if (due !== null && due < Date.now() && g.openCount > 0) return 'overdue'
  if (due !== null && due - Date.now() <= store.atRiskDays * DAY && g.openCount > 0) return 'at-risk'
  return 'ontrack'
}
const MS_WEIGHT: Record<MsStatus, number> = { overdue: 0, 'at-risk': 1, ontrack: 2, none: 3 }

const filteredMilestones = computed<MilestoneGroup[]>(() => {
  const src = store.board?.milestones ?? []
  const filtered = src
    .map((g) => ({ ...g, issues: g.issues.filter((i) => repoActive(i.repo)) }))
    .filter((g) => g.issues.length > 0)
  return filtered.sort((a, b) => MS_WEIGHT[msStatus(a)] - MS_WEIGHT[msStatus(b)])
})

function relativeTime(iso: string | null): string {
  if (!iso) return ''
  const then = new Date(iso).getTime()
  const mins = Math.round((Date.now() - then) / 60000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hours = Math.round(mins / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.round(hours / 24)
  return `${days}d ago`
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0 gap-4">
      <div class="flex items-center gap-2">
        <BackToRunButton />
        <span class="text-muted-foreground/40">/</span>
        <h1 class="text-sm font-semibold">Issues by Milestone</h1>
        <span
          v-if="!store.loading"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.summary.open }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <label class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          Stalled >
          <input
            v-model.number="store.staleDays"
            type="number"
            class="w-14 rounded border border-border/60 bg-transparent px-1.5 py-0.5 text-[11px]"
          />
          days
        </label>
        <Button variant="ghost" :disabled="store.loading" @click="store.refresh(projectId)">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="2" />
          Refresh
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6 space-y-4">
      <!-- Loading -->
      <div v-if="store.loading && !store.board" class="flex flex-col gap-3">
        <div v-for="i in 4" :key="i" class="h-24 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <template v-else-if="store.board">
        <!-- Banners -->
        <div
          v-if="!store.board.boardLinked"
          class="flex items-center gap-2 p-3 rounded-lg text-[12px] bg-primary/10 border border-primary/20 text-foreground"
        >
          <Info class="h-4 w-4 text-primary shrink-0" />
          No GitHub Project board linked — Start date / Deadline / Status will be empty. Configure it in
          <RouterLink :to="`/projects/${projectId}/settings`" class="text-primary hover:underline font-medium">Settings → GitHub Project</RouterLink>.
        </div>
        <div
          v-if="store.board.skippedNonGithub"
          class="flex items-center gap-2 p-3 rounded-lg text-[12px] bg-amber-500/10 border border-amber-500/20"
        >
          <AlertTriangle class="h-4 w-4 text-amber-500 shrink-0" />
          Skipped non-GitHub repos (GitLab not supported yet).
        </div>
        <div
          v-if="store.board.truncated"
          class="flex items-center gap-2 p-3 rounded-lg text-[12px] bg-amber-500/10 border border-amber-500/20"
        >
          <AlertTriangle class="h-4 w-4 text-amber-500 shrink-0" />
          List truncated (a repo has too many open issues).
        </div>

        <!-- Summary cards -->
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">Open issues</div>
            <div class="text-xl font-semibold mt-0.5">{{ store.summary.open }}</div>
          </Card>
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">Overdue</div>
            <div class="text-xl font-semibold mt-0.5" :class="store.summary.overdue > 0 ? 'text-red-500' : ''">{{ store.summary.overdue }}</div>
          </Card>
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">At risk</div>
            <div class="text-xl font-semibold mt-0.5" :class="store.summary.atRisk > 0 ? 'text-amber-500' : ''">{{ store.summary.atRisk }}</div>
          </Card>
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">Stalled</div>
            <div class="text-xl font-semibold mt-0.5" :class="store.summary.stale > 0 ? 'text-yellow-500' : ''">{{ store.summary.stale }}</div>
          </Card>
        </div>

        <!-- Repo filter -->
        <div v-if="allRepos.length > 1" class="flex items-center gap-1.5 flex-wrap">
          <span class="text-[11px] text-muted-foreground mr-1">Repos:</span>
          <button
            v-for="repo in allRepos" :key="repo"
            class="rounded-full border px-2.5 py-0.5 text-[11px] transition-colors"
            :class="repoActive(repo)
              ? 'border-primary/40 bg-primary/10 text-primary'
              : 'border-border/60 text-muted-foreground hover:text-foreground'"
            @click="toggleRepo(repo)"
          >
            {{ repo }}
          </button>
        </div>

        <!-- Empty -->
        <div v-if="filteredMilestones.length === 0" class="flex flex-col items-center justify-center min-h-60 text-center">
          <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
            <CircleDot class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <p class="text-sm font-medium">No open issues</p>
          <p class="text-xs text-muted-foreground mt-1">All issues are closed, or the project has no GitHub repos.</p>
        </div>

        <!-- Gantt -->
        <GanttChart v-else :milestones="filteredMilestones" />

        <div class="text-[10px] text-muted-foreground/60 text-right pt-2">
          Updated {{ relativeTime(store.lastRefreshedAt) }}
        </div>
      </template>
    </div>
  </div>
</template>
