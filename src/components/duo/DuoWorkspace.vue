<script setup lang="ts">
/**
 * Duo workspace shell — header (phase + run controls), the config form, and the
 * merged conversation.
 *
 * Layout: the two sessions used to sit in two side-by-side stream columns; they
 * now share ONE timeline ({@link DuoConversation}) so an exchange reads
 * top-to-bottom like a chat between the two agents. Config collapses to a chip
 * bar ({@link DuoSummaryBar}) once a duo is running so the conversation owns the
 * height.
 *
 * Pairs with {@link DuoHistoryList} (the switcher). Both talk to the shared
 * orchestrator store, so the list can trigger restore/new and this component
 * re-syncs via watchers on the store.
 *
 * `projectId` prop: when given, the duo is locked to that project — used when
 * embedded in the project screen. When omitted, the setup panel shows a project
 * selector (standalone route).
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Square, StepForward, CheckCircle2, AlertTriangle, Loader2, ShieldQuestion, Settings2, Users } from 'lucide-vue-next'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useOrchestratorStore } from '@/stores/orchestrator'
import { useMarkdown } from '@/lib/markdown'
import { Button } from '@/components/ui'
import DuoSetupPanel from './DuoSetupPanel.vue'
import DuoSummaryBar from './DuoSummaryBar.vue'
import DuoConversation from './DuoConversation.vue'

const props = defineProps<{ projectId?: string }>()
const emit = defineEmits<{ openSession: [runId: string] }>()

const { t } = useI18n()
const live = useLiveRunsStore()
const orch = useOrchestratorStore()
const { renderText, loadMarkdown } = useMarkdown()

const isActive = computed(() => orch.state.active)
const phase = computed(() => orch.state.phase)
const settingsOpen = ref(true)

/** True when a side is blocked on a permission / question the user must answer
 * — distinct from "working", which the spinner alone can't express. */
const awaitingInput = computed(() =>
  [orch.state.designerRunId, orch.state.reviewerRunId].some(
    (id) => !!id && !!live.get(id)?.permissionQueue.length,
  ),
)

const phaseLabel = computed(() => {
  if (awaitingInput.value) return t('duo.phase.awaitingInput')
  switch (phase.value) {
    case 'starting': return t('duo.phase.starting')
    case 'waitingDesigner': return t('duo.phase.waitingDesigner')
    case 'waitingReviewer': return t('duo.phase.waitingReviewer')
    case 'done': return t('duo.phase.done')
    case 'stopped': return t('duo.phase.stopped')
    case 'error': return t('duo.phase.error')
    default: return t('duo.phase.idle')
  }
})

const running = computed(
  () => phase.value === 'starting' || phase.value === 'waitingDesigner' || phase.value === 'waitingReviewer',
)

// Restore / "new duo" are driven from the shared store (the history list), so
// react here rather than owning the trigger.
watch(() => orch.state.id, () => { settingsOpen.value = !orch.state.active })
watch(() => orch.draftNonce, () => { settingsOpen.value = true })

const duoRunIds = computed(() =>
  [orch.state.designerRunId, orch.state.reviewerRunId].filter((id): id is string => !!id),
)

// Tell the app-wide PermissionNotifier which runs are on screen here, so it
// doesn't fire an OS notification for a question this view already shows.
watch(duoRunIds, (ids) => orch.setVisibleRunIds(ids), { immediate: true })
onBeforeUnmount(() => orch.setVisibleRunIds([]))

// Watching a turn finish HERE counts as seeing it, same as RunView does for the
// focused session — otherwise every duo turn leaves a "run finished" row in the
// Active runs dock that nothing ever clears (the user never opens the two runs
// in the Session tab).
watch(
  () => duoRunIds.value.map((id) => live.get(id)?.notifyDone ?? false),
  () => { for (const id of duoRunIds.value) live.markSeen(id) },
  { immediate: true },
)

async function stop() {
  await orch.stop()
}

async function continueRun() {
  await orch.continue()
}

onMounted(async () => {
  loadMarkdown()
  settingsOpen.value = !orch.state.active
  // Fresh (store idle) but there's saved history → auto-load the most recent
  // UNFINISHED duo for this project so the user can continue right away.
  if (orch.state.phase === 'idle') {
    const latest = orch.history.find(
      (h) => h.phase !== 'done' && (!props.projectId || h.projectId === props.projectId),
    )
    if (latest) await orch.restore(latest.id)
  }
})
</script>

<template>
  <div class="flex h-full min-w-0 flex-col overflow-hidden">
    <!-- Header. The project screen already owns the tall (h-13) header above
         this one, so match the session screen's panel bar instead: h-8, the
         same row height as the rail's tab bar. -->
    <div class="flex items-center gap-2 px-3 h-8 bg-card border-b border-border shrink-0">
      <Users class="h-3.5 w-3.5 text-foreground/40 shrink-0" :stroke-width="1.5" />
      <span class="text-xs font-medium text-foreground/90 truncate">{{ t('duo.title') }}</span>

      <span class="flex items-center gap-1.5 text-[11px] shrink-0">
        <ShieldQuestion v-if="awaitingInput" class="h-3.5 w-3.5 text-amber-500" />
        <Loader2 v-else-if="running" class="h-3.5 w-3.5 animate-spin text-primary" />
        <CheckCircle2 v-else-if="phase === 'done'" class="h-3.5 w-3.5 text-emerald-500" />
        <AlertTriangle v-else-if="phase === 'error'" class="h-3.5 w-3.5 text-amber-500" />
        <span class="text-foreground/70">{{ phaseLabel }}</span>
        <span v-if="isActive || orch.state.round > 0" class="text-muted-foreground font-mono">
          {{ t('duo.roundCounter', { n: orch.state.round, max: orch.state.maxRounds }) }}
        </span>
      </span>

      <div class="ml-auto flex items-center gap-1.5 shrink-0">
        <Button v-if="isActive" variant="destructive" size="xs" @click="stop">
          <Square class="h-3 w-3" :stroke-width="2" fill="currentColor" />
          {{ t('duo.action.stop') }}
        </Button>
        <Button v-if="!isActive && orch.state.resumable" size="xs" @click="continueRun">
          <StepForward class="h-3 w-3" :stroke-width="2" />
          {{ t('duo.action.continue') }}
        </Button>
        <button
          type="button"
          class="flex items-center gap-1 rounded h-6 px-1.5 text-[11px] font-medium transition-colors cursor-pointer"
          :class="settingsOpen ? 'bg-primary/15 text-primary' : 'text-foreground/50 hover:text-foreground/80 hover:bg-accent/60'"
          :aria-pressed="settingsOpen"
          @click="settingsOpen = !settingsOpen"
        >
          <Settings2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('duo.action.expand') }}
        </button>
      </div>
    </div>

    <DuoSetupPanel
      v-show="settingsOpen"
      :project-id="projectId"
      @started="settingsOpen = false"
    />
    <DuoSummaryBar v-if="!settingsOpen && phase !== 'idle'" @expand="settingsOpen = true" />

    <DuoConversation :render-text="renderText" @open-session="(id) => emit('openSession', id)" />
  </div>
</template>
