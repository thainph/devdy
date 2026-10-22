<script setup lang="ts">
// Project Pulse screen (docs/project-pulse-spec.md §6.1), master–detail: the
// project list on the left in BR-09 order (red → yellow → green → none, then the
// nearest milestone, then name), the selected project's sources on the right. Opening it shows the stored digests at once —
// it never refreshes by itself (G2); the header / card / drawer buttons do.
// While mounted it keeps MCP requests polled (`pulse_set_screen_open`, BR-11).
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity, Loader2, RefreshCw } from 'lucide-vue-next'
import { Button, Card, Skeleton } from '@/components/ui'
import PulseAddWatch from '@/components/pulse/PulseAddWatch.vue'
import PulseProjectCard from '@/components/pulse/PulseProjectCard.vue'
import PulseProjectList from '@/components/pulse/PulseProjectList.vue'
import PulseWatchDrawer from '@/components/pulse/PulseWatchDrawer.vue'
import { usePulseText } from '@/components/pulse/usePulseText'
import { usePulseWatch } from '@/composables/usePulseWatch'
import { useToast } from '@/composables/useToast'
import { filterProjects, type PulseWatchItem } from '@/lib/pulse'

const { t } = useI18n()
const { toast } = useToast()
const { store, unwatch, errorMessage } = usePulseWatch()
const { clock } = usePulseText()

const query = ref('')
const attentionOnly = ref(false)
const openWatchId = ref<string | null>(null)
/** Scopes whose refresh was just requested (spinner until progress ends). */
const refreshingScopes = ref(new Set<string>())

const visible = computed(() => filterProjects(store.projects, query.value, attentionOnly.value))

// Master–detail: the remembered project, else the first (most urgent) visible one.
const SELECTED_KEY = 'devdy:pulseProjectId'
const selectedId = ref<string | null>(localStorage.getItem(SELECTED_KEY))
const selected = computed(() => visible.value.find((p) => p.projectId === selectedId.value) ?? visible.value[0] ?? null)
function select(projectId: string) {
  selectedId.value = projectId
  localStorage.setItem(SELECTED_KEY, projectId)
}
const totalWatches = computed(() => store.projects.reduce((n, p) => n + p.watches.length, 0))
const openWatch = computed<PulseWatchItem | null>(() => {
  for (const p of store.projects) {
    const w = p.watches.find((x) => x.id === openWatchId.value)
    if (w) return w
  }
  return null
})
const refreshingAny = computed(() => !!store.progress)

onMounted(async () => {
  store.setScreenOpen(true)
  await store.start()
  // §3.3 exception: a source that was never summarised is summarised on open
  // Pending ones are still being resolved (which summarises them anyway);
  // failed ones wait for an explicit Refresh instead of retrying on every open.
  for (const p of store.projects) {
    for (const w of p.watches) {
      if (!w.digest && w.resolveStatus === 'ready' && !w.lastError) {
        void store.refresh({ watchId: w.id }).catch(() => {})
      }
    }
  }
})
onBeforeUnmount(() => store.setScreenOpen(false))

async function refresh(scope: { projectId?: string; watchId?: string }, key: string) {
  refreshingScopes.value = new Set(refreshingScopes.value).add(key)
  try {
    await store.refresh(scope)
  } catch (e) {
    toast.error(errorMessage(e))
  } finally {
    // The progress chip takes over; per-scope spinners only bridge the gap.
    setTimeout(() => {
      const next = new Set(refreshingScopes.value)
      next.delete(key)
      refreshingScopes.value = next
    }, 800)
  }
}

const busy = (key: string) => refreshingAny.value || refreshingScopes.value.has(key)

async function onUnwatch() {
  const w = openWatch.value
  if (w && (await unwatch(w.id))) openWatchId.value = null
}

async function onMove(projectId: string) {
  const w = openWatch.value
  if (!w) return
  try {
    const moved = await store.move(w.id, projectId)
    toast.success(t('pulse.watch.moved', { project: moved.projectName }))
  } catch (e) {
    toast.error(errorMessage(e))
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0 gap-3">
      <div class="flex items-center gap-2 min-w-0">
        <h1 class="text-sm font-semibold shrink-0">{{ t('pulse.title') }}</h1>
        <span
          v-if="store.loaded && store.projects.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground shrink-0"
        >
          {{ store.projects.length }}
        </span>
        <span
          v-if="store.progress"
          class="flex items-center gap-1 rounded-full border border-primary/30 bg-primary/10 px-2 py-0.5 text-[10px] font-medium text-primary shrink-0"
        >
          <Loader2 class="h-3 w-3 animate-spin" :stroke-width="2" />
          {{ t('pulse.refreshing', { done: store.progress.done, total: store.progress.total }) }}
        </span>
        <span v-else-if="store.updatedAt" class="truncate text-[11px] text-muted-foreground">
          {{ t('pulse.updatedAt', { time: clock(store.updatedAt) }) }}
        </span>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <PulseAddWatch />
        <Button
          size="sm"
          :disabled="busy('all') || totalWatches === 0"
          :title="t('pulse.refreshAllHint')"
          @click="refresh({}, 'all')"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': busy('all') }" :stroke-width="1.75" />
          {{ t('pulse.refreshAll') }}
        </Button>
      </div>
    </div>

    <!-- First load -->
    <div v-if="!store.loaded && store.loading" class="flex flex-1 min-h-0">
      <div class="w-[260px] shrink-0 space-y-2 border-r border-border/60 bg-sidebar p-3">
        <Skeleton class="h-8 w-full rounded" />
        <Skeleton v-for="i in 6" :key="i" class="h-9 w-full rounded" />
      </div>
      <div class="flex-1 space-y-3 p-6">
        <Skeleton class="h-5 w-48 rounded" />
        <Skeleton class="h-3 w-64 rounded" />
        <Skeleton v-for="r in 4" :key="r" class="h-10 w-full rounded" />
      </div>
    </div>

    <!-- Global error -->
    <div v-else-if="store.error && !store.loaded" class="p-6">
      <Card class="border-destructive/30" body-class="flex items-center gap-3 p-4">
        <p class="flex-1 text-sm text-destructive">{{ t('pulse.loadError') }}: {{ store.error }}</p>
        <Button variant="outline" size="sm" @click="store.load()">{{ t('pulse.retry') }}</Button>
      </Card>
    </div>

    <!-- Empty -->
    <div v-else-if="store.projects.length === 0" class="flex flex-1 flex-col items-center justify-center min-h-80 text-center">
      <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
        <Activity class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-sm font-medium">{{ t('pulse.empty.title') }}</p>
      <p class="text-xs text-muted-foreground mt-1 max-w-96">{{ t('pulse.empty.hint') }}</p>
    </div>

    <!-- Master–detail -->
    <div v-else class="flex flex-1 min-h-0">
      <PulseProjectList
        v-model:query="query"
        v-model:attention-only="attentionOnly"
        :projects="visible"
        :selected-id="selected?.projectId ?? null"
        :total="store.projects.length"
        @select="select"
      />
      <div class="flex-1 min-w-0 overflow-y-auto p-6">
        <PulseProjectCard
          v-if="selected"
          :key="selected.projectId"
          :project="selected"
          full
          :refreshing="busy(`project:${selected.projectId}`)"
          @refresh="refresh({ projectId: selected.projectId }, `project:${selected.projectId}`)"
          @open="openWatchId = $event.id"
        />
        <p v-else class="py-12 text-center text-sm text-muted-foreground">{{ t('pulse.empty.filtered') }}</p>
      </div>
    </div>

    <PulseWatchDrawer
      :watch="openWatch"
      :refreshing="openWatch ? busy(`watch:${openWatch.id}`) : false"
      @close="openWatchId = null"
      @refresh="openWatch && refresh({ watchId: openWatch.id }, `watch:${openWatch.id}`)"
      @move="onMove"
      @unwatch="onUnwatch"
    />
  </div>
</template>
