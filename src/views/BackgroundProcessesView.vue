<script setup lang="ts">
// Background processes manager: every process Devdy tracks (sessions, its own
// helpers, and processes left behind by ended sessions or a previous app run),
// with graceful stop / force kill per entry or per process and bulk cleanup.
//
// Not in App.vue's KeepAlive list: it polls while shown, so the timer must end
// on unmount.
import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { ChevronRight, Cpu, RefreshCw, Trash2 } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import ProcessEntryRow from '@/components/processes/ProcessEntryRow.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { focusSessionWindow } from '@/lib/sessionWindow'
import { groupEntries, strayIds, type BgEntry } from '@/lib/backgroundProcesses'

defineOptions({ name: 'BackgroundProcessesView' })

const { t } = useI18n()
const router = useRouter()
const store = useBackgroundProcessesStore()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const tabsStore = useWorkspaceTabsStore()
const { confirm } = useConfirm()
const { toast } = useToast()

const entries = computed(() => store.snapshot?.entries ?? [])
const groups = computed(() => groupEntries(entries.value))
const strays = computed(() => strayIds(entries.value))
const totalProcesses = computed(() => entries.value.reduce((n, e) => n + e.processes.length, 0))
const strayProcesses = computed(() =>
  entries.value.filter((e) => e.state !== 'active').reduce((n, e) => n + e.processes.length, 0),
)

// App services are plumbing: collapsed unless the user asks.
const servicesOpen = ref(false)

// Ages tick with each snapshot rather than on their own timer.
const now = ref(Date.now())
watch(
  () => store.snapshot,
  () => (now.value = Date.now()),
)

const busy = reactive(new Set<string>())
const bulkBusy = ref(false)

let endWatch: (() => void) | null = null
onMounted(() => {
  endWatch = store.watch(3000)
})
onUnmounted(() => {
  endWatch?.()
  endWatch = null
})

function reportStopped(n: number) {
  if (n > 0) toast.success(t('processes.toast.stopped', { n }, n))
  else toast.info(t('processes.toast.nothingStopped'))
}

async function run(ids: string[], force: boolean) {
  ids.forEach((id) => busy.add(id))
  try {
    reportStopped(await store.stopEntries(ids, force))
  } catch (e) {
    toast.error(t('processes.toast.failed', { error: String(e) }))
  } finally {
    ids.forEach((id) => busy.delete(id))
  }
}

async function stopEntry(e: BgEntry) {
  // Stopping an active session also cancels the run — worth one confirmation.
  if (e.state === 'active' && e.kind === 'run') {
    const ok = await confirm({
      title: t('processes.confirm.stopSessionTitle'),
      message: t('processes.confirm.stopSessionMessage', { label: e.label }),
      confirmLabel: t('processes.actions.stop'),
    })
    if (!ok) return
  }
  await run([e.id], false)
}

async function forceEntry(e: BgEntry) {
  const ok = await confirm({
    title: t('processes.confirm.forceTitle'),
    message: t('processes.confirm.forceMessage', { label: e.label }),
    confirmLabel: t('processes.actions.forceKill'),
  })
  if (ok) await run([e.id], true)
}

async function killPid(e: BgEntry, pid: number) {
  busy.add(e.id)
  try {
    await store.killProcess(pid)
  } catch (err) {
    toast.error(t('processes.toast.failed', { error: String(err) }))
  } finally {
    busy.delete(e.id)
  }
}

async function stopLeftovers() {
  const n = strayProcesses.value
  const ok = await confirm({
    title: t('processes.confirm.stopLeftoversTitle'),
    message: t('processes.confirm.stopLeftoversMessage', { n }, n),
    confirmLabel: t('processes.stopAllLeftovers'),
  })
  if (!ok) return
  bulkBusy.value = true
  try {
    await run(strays.value, false)
  } finally {
    bulkBusy.value = false
  }
}

async function stopEverything() {
  const ok = await confirm({
    title: t('processes.confirm.stopAllTitle'),
    message: t('processes.confirm.stopAllMessage', { n: totalProcesses.value }),
    confirmLabel: t('processes.stopAll'),
  })
  if (!ok) return
  bulkBusy.value = true
  try {
    await run(entries.value.map((e) => e.id), false)
  } finally {
    bulkBusy.value = false
  }
}

// Same path as the Active runs dock / tray: the run's own pop-out if it has one,
// otherwise its workspace tab in this window.
async function openSession(e: BgEntry) {
  const runId = e.run_id
  if (!runId) return
  if (await focusSessionWindow(runId)) return
  const projectId = e.project_id ?? runsStore.runMeta.get(runId)?.project_id ?? null
  if (!projectId) return
  live.markSeen(runId)
  tabsStore.open(projectId, runId)
  router.push({ name: 'project-run-detail', params: { projectId, runId } }).catch(() => {})
}

const sections = computed(() => [
  { key: 'orphan', title: t('processes.sections.orphan'), hint: t('processes.sections.orphanHint'), items: groups.value.orphan, warn: true },
  { key: 'leftover', title: t('processes.sections.leftover'), hint: t('processes.sections.leftoverHint'), items: groups.value.leftover, warn: true },
  { key: 'sessions', title: t('processes.sections.sessions'), hint: '', items: groups.value.sessions, warn: false },
])
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">{{ t('processes.title') }}</h1>
        <span
          v-if="totalProcesses > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ totalProcesses }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button variant="ghost" size="icon-sm" :title="t('processes.refresh')" :aria-label="t('processes.refresh')" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          v-if="strays.length > 0"
          variant="destructive"
          :disabled="bulkBusy"
          @click="stopLeftovers"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('processes.stopAllLeftovers') }}
        </Button>
        <Button v-if="entries.length > 0" variant="outline" :disabled="bulkBusy" @click="stopEverything">
          {{ t('processes.stopAll') }}
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Loading skeleton (first snapshot only) -->
      <div v-if="store.loading && !store.snapshot" class="max-w-4xl space-y-3">
        <div v-for="i in 3" :key="i" class="h-20 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error with nothing to show -->
      <div
        v-else-if="store.error && !store.snapshot"
        class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20"
      >
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="entries.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <Cpu class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('processes.empty.title') }}</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-80">{{ t('processes.empty.hint') }}</p>
      </div>

      <div v-else class="max-w-4xl space-y-4">
        <p class="text-xs text-muted-foreground">{{ t('processes.subtitle') }}</p>

        <template v-for="s in sections" :key="s.key">
          <Card v-if="s.items.length > 0" body-class="divide-y divide-border/50">
            <template #header>
              <span
                class="h-2 w-2 shrink-0 rounded-full"
                :class="s.warn ? 'bg-amber-500' : 'bg-primary'"
              />
              <span class="text-xs font-semibold">{{ s.title }}</span>
              <span class="text-[11px] text-muted-foreground">({{ s.items.length }})</span>
              <span v-if="s.hint" class="ml-2 truncate text-[11px] text-muted-foreground">{{ s.hint }}</span>
            </template>
            <ProcessEntryRow
              v-for="e in s.items"
              :key="e.id"
              :entry="e"
              :now="now"
              :busy="busy.has(e.id) || bulkBusy"
              @stop="stopEntry(e)"
              @force="forceEntry(e)"
              @open="openSession(e)"
              @kill-pid="(pid) => killPid(e, pid)"
            />
          </Card>
        </template>

        <!-- App services: collapsed by default -->
        <Card v-if="groups.services.length > 0" body-class="divide-y divide-border/50">
          <template #header>
            <button
              type="button"
              class="flex min-w-0 flex-1 items-center gap-2 text-left cursor-pointer"
              :aria-expanded="servicesOpen"
              @click="servicesOpen = !servicesOpen"
            >
              <ChevronRight
                class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
                :class="servicesOpen && 'rotate-90'"
                :stroke-width="2"
              />
              <span class="text-xs font-semibold">{{ t('processes.sections.services') }}</span>
              <span class="text-[11px] text-muted-foreground">({{ groups.services.length }})</span>
              <span class="ml-2 truncate text-[11px] text-muted-foreground">{{ t('processes.sections.servicesHint') }}</span>
            </button>
          </template>
          <template v-if="servicesOpen">
            <ProcessEntryRow
              v-for="e in groups.services"
              :key="e.id"
              :entry="e"
              :now="now"
              :busy="busy.has(e.id) || bulkBusy"
              @stop="stopEntry(e)"
              @force="forceEntry(e)"
              @open="openSession(e)"
              @kill-pid="(pid) => killPid(e, pid)"
            />
          </template>
        </Card>
      </div>
    </div>
  </div>
</template>
