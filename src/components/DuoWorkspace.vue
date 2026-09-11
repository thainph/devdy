<script setup lang="ts">
/**
 * Duo workspace — the config form + the two side-by-side live streams. Pairs
 * with {@link DuoHistoryList} (the switcher). Both talk to the shared
 * orchestrator store, so the list can trigger restore/new and this component
 * re-syncs its form via watchers on the store.
 *
 * `projectId` prop: when given, the duo is locked to that project (the project
 * selector is hidden) — used when embedded in the project screen. When omitted,
 * a project selector is shown (standalone route).
 */
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Play, Square, RotateCcw, StepForward, ChevronDown, ChevronUp, CheckCircle2, AlertTriangle, Loader2, ExternalLink } from 'lucide-vue-next'
import { useProjectsStore } from '@/stores/projects'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useOrchestratorStore, type DuoRole, type DuoSourceMode } from '@/stores/orchestrator'
import { modelOptionsFor, PERMISSION_MODE_OPTIONS } from '@/lib/engineOptions'
import { useMarkdown } from '@/lib/markdown'
import { useToast } from '@/composables/useToast'
import StreamLog from '@/components/StreamLog.vue'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import { Button, AppSelect, Input, Textarea } from '@/components/ui'
import type { StreamEntry } from '@/lib/streamEvents'

const props = defineProps<{ projectId?: string }>()
const emit = defineEmits<{ openSession: [runId: string] }>()

const { t } = useI18n()
const projectsStore = useProjectsStore()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const orch = useOrchestratorStore()
const { toast } = useToast()
const { renderText, loadMarkdown } = useMarkdown()

const ENGINE_OPTIONS = [
  { value: 'claude', label: 'claude' },
  { value: 'codex', label: 'codex' },
]

const lockedProject = computed(() => !!props.projectId)

// ── Config form ────────────────────────────────────────────────────────────
const projectId = ref(props.projectId ?? '')
const sourceMode = ref<DuoSourceMode>('new')
const existingDesignerId = ref('')
const existingReviewerId = ref('')
const designerEngine = ref('claude')
const reviewerEngine = ref('codex')
const designerModel = ref('')
const reviewerModel = ref('')
const designerLabel = ref('')
const reviewerLabel = ref('')
const designerInstruction = ref('')
const reviewerInstruction = ref('')
const permissionMode = ref('acceptEdits')
const goal = ref('')
const maxRounds = ref(6)
const consensusToken = ref('[APPROVED]')
const overrideBudget = ref(false)
const starting = ref(false)
const settingsOpen = ref(true)

const projectOptions = computed(() =>
  projectsStore.projects.map((p) => ({ value: p.id, label: p.name })),
)
const designerModelOptions = computed(() => modelOptionsFor(designerEngine.value))
const reviewerModelOptions = computed(() => modelOptionsFor(reviewerEngine.value))

function formatWhen(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return d.toLocaleString([], { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
}
const runOptions = computed(() => {
  const runTypeLabels: Record<string, string> = {
    session: t('duo.runType.session'),
    analyze_issue: t('duo.runType.issue'),
    review_pr: t('duo.runType.pr'),
  }
  return runsStore.runs.map((r) => {
    const kind = runTypeLabels[r.run_type] || r.run_type
    const label = r.title?.trim() || (r.ref_number ? `${kind} #${r.ref_number}` : kind)
    const when = formatWhen(r.finished_at || r.started_at || r.created_at)
    const parts = [r.engine, r.status, when, `#${r.id.slice(0, 6)}`].filter(Boolean)
    return { value: r.id, label: `${r.pinned ? '📌 ' : ''}${label}`, description: parts.join(' · ') }
  })
})

const isActive = computed(() => orch.state.active)
const phase = computed(() => orch.state.phase)

const canStart = computed(() => {
  if (isActive.value || starting.value || !projectId.value) return false
  if (sourceMode.value === 'existing') {
    return (
      !!existingDesignerId.value &&
      !!existingReviewerId.value &&
      existingDesignerId.value !== existingReviewerId.value
    )
  }
  return goal.value.trim().length > 0
})

async function loadProjectRuns() {
  if (!projectId.value) return
  await runsStore.fetchRuns(projectId.value).catch(() => {})
}

// True while we copy orchestrator state back into the form, so the project/mode
// watcher below doesn't wipe the restored picks.
let hydrating = false

watch([projectId, sourceMode], () => {
  if (hydrating) return
  existingDesignerId.value = ''
  existingReviewerId.value = ''
  if (sourceMode.value === 'existing') void loadProjectRuns()
})

function hydrateFromState() {
  const s = orch.state
  if (s.phase === 'idle') return
  hydrating = true
  projectId.value = s.projectId
  sourceMode.value = s.mode
  designerEngine.value = s.designerEngine
  reviewerEngine.value = s.reviewerEngine
  designerModel.value = s.designerModel
  reviewerModel.value = s.reviewerModel
  permissionMode.value = s.permissionMode
  overrideBudget.value = s.overrideBudget
  designerLabel.value = s.designerLabel
  reviewerLabel.value = s.reviewerLabel
  designerInstruction.value = s.designerInstruction
  reviewerInstruction.value = s.reviewerInstruction
  goal.value = s.goal
  maxRounds.value = s.maxRounds
  consensusToken.value = s.consensusToken
  if (s.mode === 'existing') {
    existingDesignerId.value = s.designerRunId ?? ''
    existingReviewerId.value = s.reviewerRunId ?? ''
  }
  settingsOpen.value = !s.active
  nextTick(() => {
    hydrating = false
    if (s.mode === 'existing') void loadProjectRuns()
  })
}

/** Reset the config form back to editable defaults. */
function resetForm() {
  sourceMode.value = 'new'
  existingDesignerId.value = ''
  existingReviewerId.value = ''
  designerEngine.value = 'claude'
  reviewerEngine.value = 'codex'
  designerModel.value = ''
  reviewerModel.value = ''
  permissionMode.value = 'acceptEdits'
  goal.value = ''
  maxRounds.value = 6
  consensusToken.value = '[APPROVED]'
  overrideBudget.value = false
  designerLabel.value = t('duo.defaults.designerLabel')
  reviewerLabel.value = t('duo.defaults.reviewerLabel')
  designerInstruction.value = t('duo.defaults.designerInstruction')
  reviewerInstruction.value = t('duo.defaults.reviewerInstruction')
  settingsOpen.value = true
  if (lockedProject.value) projectId.value = props.projectId ?? ''
}

// The history list drives restore/new via the shared store; react to it here so
// the form + streams stay in sync no matter which component triggered it.
watch(() => orch.state.id, () => hydrateFromState())
watch(() => orch.draftNonce, () => resetForm())
watch(() => props.projectId, (id) => { if (id && !isActive.value) projectId.value = id })

const phaseLabel = computed(() => {
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

const stopReasonLabel = computed(() => {
  switch (orch.state.stopReason) {
    case 'consensus': return t('duo.reason.consensus')
    case 'maxRounds': return t('duo.reason.maxRounds')
    case 'manual': return t('duo.reason.manual')
    case 'error': return orch.state.error || t('duo.reason.error')
    default: return ''
  }
})

// ── Live stream binding ──────────────────────────────────────────────────────
function entriesFor(runId: string | null): StreamEntry[] {
  if (!runId) return []
  return live.get(runId)?.entries ?? []
}
const designerEntries = computed(() => entriesFor(orch.state.designerRunId))
const reviewerEntries = computed(() => entriesFor(orch.state.reviewerRunId))

function isRunning(role: DuoRole): boolean {
  return orch.state.active && orch.state.waiting === role
}

// Keep each stream pinned to the bottom as new output streams in. Deep-watch so
// text growing inside the last entry (not just new entries) also scrolls.
const designerScrollEl = ref<HTMLElement | null>(null)
const reviewerScrollEl = ref<HTMLElement | null>(null)
function scrollToEnd(el: HTMLElement | null) {
  if (el) el.scrollTop = el.scrollHeight
}
watch(designerEntries, () => scrollToEnd(designerScrollEl.value), { deep: true, flush: 'post' })
watch(reviewerEntries, () => scrollToEnd(reviewerScrollEl.value), { deep: true, flush: 'post' })

function openSession(runId: string | null) {
  if (runId) emit('openSession', runId)
}

// ── In-place permission / question prompts ───────────────────────────────────
// Each side's session may raise a permission request or an AskUserQuestion.
// Surface the head request right here so the user can answer without having to
// open the session tab (previously the only place the prompt was rendered).
const designerPermission = computed(() => live.get(orch.state.designerRunId ?? '')?.permissionQueue[0] ?? null)
const reviewerPermission = computed(() => live.get(orch.state.reviewerRunId ?? '')?.permissionQueue[0] ?? null)
function allowedToolsFor(runId: string | null): string[] {
  return (runId && live.get(runId)?.allowedTools) || []
}

async function handleDecide(runId: string | null, decision: 'allow' | 'deny' | 'ask', remember: boolean) {
  if (!runId) return
  const req = live.get(runId)?.permissionQueue[0]
  if (!req) return
  live.shiftPermission(runId)
  if (remember && decision === 'allow') live.rememberAllowedTool(runId, req.tool_name)
  else if (remember && decision === 'deny') live.rememberDeniedTool(runId, req.tool_name)
  try {
    await runsStore.respondPermission(req.run_id, req.request_id, decision)
  } catch (e) {
    toast.error(t('run.permissionResponseFailed', { error: String(e) }))
  }
}

async function handleAnswer(runId: string | null, answers: Record<string, string>) {
  if (!runId) return
  const req = live.get(runId)?.permissionQueue[0]
  if (!req) return
  live.shiftPermission(runId)
  try {
    await runsStore.respondPermission(req.run_id, req.request_id, 'allow', undefined, { answers })
  } catch (e) {
    toast.error(t('run.permissionResponseFailed', { error: String(e) }))
  }
}

// ── Actions ──────────────────────────────────────────────────────────────────
async function start() {
  if (!canStart.value) return
  starting.value = true
  try {
    const existing = sourceMode.value === 'existing'
    const dEngine = existing
      ? runsStore.runs.find((r) => r.id === existingDesignerId.value)?.engine || 'claude'
      : designerEngine.value
    const rEngine = existing
      ? runsStore.runs.find((r) => r.id === existingReviewerId.value)?.engine || 'codex'
      : reviewerEngine.value
    await orch.start({
      projectId: projectId.value,
      mode: sourceMode.value,
      designerEngine: dEngine,
      reviewerEngine: rEngine,
      designerModel: existing ? undefined : designerModel.value || undefined,
      reviewerModel: existing ? undefined : reviewerModel.value || undefined,
      designerRunId: existing ? existingDesignerId.value : undefined,
      reviewerRunId: existing ? existingReviewerId.value : undefined,
      designerLabel: designerLabel.value || undefined,
      reviewerLabel: reviewerLabel.value || undefined,
      designerInstruction: designerInstruction.value || undefined,
      reviewerInstruction: reviewerInstruction.value || undefined,
      permissionMode: permissionMode.value || undefined,
      goal: goal.value.trim(),
      maxRounds: maxRounds.value,
      consensusToken: consensusToken.value,
      overrideBudget: overrideBudget.value,
    })
    settingsOpen.value = false
  } catch (e) {
    toast.error(t('duo.startFailed', { error: String(e) }))
  } finally {
    starting.value = false
  }
}

async function stop() {
  await orch.stop()
}

async function continueRun() {
  orch.state.maxRounds = maxRounds.value
  await orch.continue()
}

function newDuo() {
  if (isActive.value) return
  orch.newDraft()
}

function onDesignerEngineChange(v: string) {
  designerEngine.value = v
  if (!modelOptionsFor(v).some((o) => o.value === designerModel.value)) designerModel.value = ''
}
function onReviewerEngineChange(v: string) {
  reviewerEngine.value = v
  if (!modelOptionsFor(v).some((o) => o.value === reviewerModel.value)) reviewerModel.value = ''
}

onMounted(async () => {
  loadMarkdown()
  // Restore an in-flight / paused / finished duo so this screen shows the same
  // settings + streams and lets the user Stop/Continue.
  hydrateFromState()
  // Fresh (store idle) but there's saved history → auto-load the most recent
  // UNFINISHED duo for this project so the user can continue right away.
  if (orch.state.phase === 'idle') {
    const latest = orch.history.find(
      (h) => h.phase !== 'done' && (!props.projectId || h.projectId === props.projectId),
    )
    if (latest) await orch.restore(latest.id)
  }
  if (!designerLabel.value) designerLabel.value = t('duo.defaults.designerLabel')
  if (!reviewerLabel.value) reviewerLabel.value = t('duo.defaults.reviewerLabel')
  if (!designerInstruction.value) designerInstruction.value = t('duo.defaults.designerInstruction')
  if (!reviewerInstruction.value) reviewerInstruction.value = t('duo.defaults.reviewerInstruction')
  if (!projectsStore.projects.length) await projectsStore.fetchProjects().catch(() => {})
  if (!projectId.value && !lockedProject.value && projectsStore.projects.length) {
    projectId.value = projectsStore.projects[0].id
  }
})
</script>

<template>
  <div class="flex h-full min-w-0 flex-col overflow-hidden">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <h1 class="text-sm font-semibold">{{ t('duo.title') }}</h1>

      <div class="flex items-center gap-3">
        <div class="flex items-center gap-2 text-xs">
          <Loader2 v-if="phase === 'starting' || phase === 'waitingDesigner' || phase === 'waitingReviewer'"
            class="h-4 w-4 animate-spin text-primary" />
          <CheckCircle2 v-else-if="phase === 'done'" class="h-4 w-4 text-emerald-500" />
          <AlertTriangle v-else-if="phase === 'error'" class="h-4 w-4 text-amber-500" />
          <span class="font-medium">{{ phaseLabel }}</span>
          <span v-if="isActive || orch.state.round > 0" class="text-muted-foreground font-mono">
            {{ t('duo.roundCounter', { n: orch.state.round, max: orch.state.maxRounds }) }}
          </span>
        </div>
        <Button v-if="isActive" variant="destructive" size="sm" @click="stop">
          <Square class="h-3.5 w-3.5" :stroke-width="2" fill="currentColor" />
          {{ t('duo.action.stop') }}
        </Button>
        <Button v-if="!isActive && orch.state.resumable" size="sm" @click="continueRun">
          <StepForward class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('duo.action.continue') }}
        </Button>
        <Button variant="ghost" size="sm" @click="settingsOpen = !settingsOpen">
          <component :is="settingsOpen ? ChevronUp : ChevronDown" class="h-3.5 w-3.5" :stroke-width="2" />
          {{ settingsOpen ? t('duo.action.collapse') : t('duo.action.expand') }}
        </Button>
      </div>
    </div>

    <!-- Config bar -->
    <section v-show="settingsOpen" class="border-b border-border bg-card/40 px-4 py-3 space-y-3 shrink-0">
      <div class="inline-flex rounded-md border border-border p-0.5 text-xs">
        <button
          class="px-3 py-1 rounded-[5px] transition-colors cursor-pointer"
          :class="sourceMode === 'new' ? 'bg-accent text-foreground font-medium' : 'text-muted-foreground hover:text-foreground'"
          :disabled="isActive"
          @click="sourceMode = 'new'"
        >{{ t('duo.mode.new') }}</button>
        <button
          class="px-3 py-1 rounded-[5px] transition-colors cursor-pointer"
          :class="sourceMode === 'existing' ? 'bg-accent text-foreground font-medium' : 'text-muted-foreground hover:text-foreground'"
          :disabled="isActive"
          @click="sourceMode = 'existing'"
        >{{ t('duo.mode.existing') }}</button>
      </div>

      <div class="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-4">
        <label v-if="!lockedProject" class="flex flex-col gap-1">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.project') }}</span>
          <AppSelect v-model="projectId" :options="projectOptions" :disabled="isActive"
            :placeholder="t('duo.field.projectPlaceholder')" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.permission') }}</span>
          <AppSelect v-model="permissionMode" :options="PERMISSION_MODE_OPTIONS" :disabled="isActive" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.maxRounds') }}</span>
          <Input v-model.number="maxRounds" type="number" min="1" max="30" :disabled="isActive" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.consensusToken') }}</span>
          <Input v-model="consensusToken" :disabled="isActive" placeholder="[APPROVED]" />
        </label>
      </div>

      <!-- New sessions: engine + model per side -->
      <div v-if="sourceMode === 'new'" class="grid grid-cols-1 gap-3 md:grid-cols-2">
        <div class="flex items-end gap-2">
          <label class="flex flex-1 flex-col gap-1 min-w-0">
            <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.designerEngine') }}</span>
            <AppSelect :model-value="designerEngine" :options="ENGINE_OPTIONS" :disabled="isActive"
              @update:model-value="onDesignerEngineChange" />
          </label>
          <label class="flex flex-1 flex-col gap-1 min-w-0">
            <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.model') }}</span>
            <AppSelect v-model="designerModel" :options="designerModelOptions" :disabled="isActive" />
          </label>
        </div>
        <div class="flex items-end gap-2">
          <label class="flex flex-1 flex-col gap-1 min-w-0">
            <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.reviewerEngine') }}</span>
            <AppSelect :model-value="reviewerEngine" :options="ENGINE_OPTIONS" :disabled="isActive"
              @update:model-value="onReviewerEngineChange" />
          </label>
          <label class="flex flex-1 flex-col gap-1 min-w-0">
            <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.model') }}</span>
            <AppSelect v-model="reviewerModel" :options="reviewerModelOptions" :disabled="isActive" />
          </label>
        </div>
      </div>

      <!-- Existing sessions: pick two runs from the project -->
      <div v-else class="grid grid-cols-1 gap-3 md:grid-cols-2">
        <label class="flex flex-col gap-1">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.designerRun') }}</span>
          <AppSelect v-model="existingDesignerId" :options="runOptions" :disabled="isActive"
            :placeholder="t('duo.field.runPlaceholder')" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.reviewerRun') }}</span>
          <AppSelect v-model="existingReviewerId" :options="runOptions" :disabled="isActive"
            :placeholder="t('duo.field.runPlaceholder')" />
        </label>
        <p v-if="!runOptions.length" class="md:col-span-2 text-[11px] text-muted-foreground">
          {{ t('duo.noRuns') }}
        </p>
        <p v-else-if="existingDesignerId && existingDesignerId === existingReviewerId"
          class="md:col-span-2 text-[11px] text-amber-600 dark:text-amber-400">
          {{ t('duo.sameRun') }}
        </p>
        <p v-else class="md:col-span-2 text-[11px] text-muted-foreground">
          {{ t('duo.existingHint') }}
        </p>
      </div>

      <!-- Roles -->
      <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
        <div class="space-y-1.5">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.sideA') }}</span>
          <Input v-model="designerLabel" :disabled="isActive" :placeholder="t('duo.field.labelPlaceholder')" />
          <Textarea v-model="designerInstruction" :disabled="isActive" :rows="2"
            :placeholder="t('duo.field.instructionPlaceholder')" />
        </div>
        <div class="space-y-1.5">
          <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.sideB') }}</span>
          <Input v-model="reviewerLabel" :disabled="isActive" :placeholder="t('duo.field.labelPlaceholder')" />
          <Textarea v-model="reviewerInstruction" :disabled="isActive" :rows="2"
            :placeholder="t('duo.field.instructionPlaceholder')" />
        </div>
      </div>

      <label class="flex flex-col gap-1">
        <span class="text-[11px] font-medium text-muted-foreground">{{ t('duo.field.goal') }}</span>
        <Textarea v-model="goal" :disabled="isActive" :rows="3" :placeholder="t('duo.field.goalPlaceholder')" />
      </label>

      <div class="flex flex-wrap items-center gap-3">
        <Button v-if="!isActive" :disabled="!canStart" @click="start">
          <Loader2 v-if="starting" class="h-3.5 w-3.5 animate-spin" />
          <Play v-else class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('duo.action.start') }}
        </Button>
        <Button v-if="!isActive && (orch.state.turns.length || phase !== 'idle')" variant="ghost" @click="newDuo">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('duo.action.reset') }}
        </Button>

        <label class="flex items-center gap-1.5 text-xs text-muted-foreground ml-auto">
          <input v-model="overrideBudget" type="checkbox" :disabled="isActive" class="accent-primary" />
          {{ t('duo.field.overrideBudget') }}
        </label>
      </div>

      <p v-if="stopReasonLabel"
        class="text-xs rounded-md px-3 py-2"
        :class="phase === 'done' ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
          : phase === 'error' ? 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
          : 'bg-muted text-muted-foreground'">
        {{ stopReasonLabel }}
      </p>
    </section>

    <!-- Dual streams -->
    <section class="grid flex-1 grid-cols-1 gap-px overflow-hidden bg-border md:grid-cols-2">
      <div class="flex min-h-0 flex-col overflow-hidden bg-background">
        <div class="flex items-center gap-2 border-b border-border px-3 py-2 shrink-0">
          <span class="text-xs font-semibold">{{ orch.state.designerLabel }}</span>
          <span class="text-[11px] text-muted-foreground font-mono">{{ orch.state.designerEngine }}</span>
          <div class="ml-auto flex items-center gap-1.5">
            <Loader2 v-if="isRunning('designer')" class="h-3.5 w-3.5 animate-spin text-primary" />
            <button v-if="orch.state.designerRunId"
              class="inline-flex items-center gap-1 h-6 px-1.5 rounded text-[11px] text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
              :title="t('duo.openSession')"
              @click="openSession(orch.state.designerRunId)"
            >
              <ExternalLink class="h-3.5 w-3.5" :stroke-width="2" />
              {{ t('duo.openSession') }}
            </button>
          </div>
        </div>
        <div class="relative flex-1 min-h-0">
          <div ref="designerScrollEl" class="absolute inset-0 overflow-y-auto px-3 py-2">
            <StreamLog v-if="designerEntries.length" :entries="designerEntries"
              :running="isRunning('designer')" :render-text="renderText" />
            <p v-else class="text-xs text-muted-foreground p-4 text-center">{{ t('duo.emptyDesigner') }}</p>
          </div>
          <div v-if="designerPermission"
            class="absolute inset-0 z-20 overflow-auto bg-card border-t border-border">
            <PermissionPrompt
              :key="designerPermission.request_id"
              :request="designerPermission"
              :allowed-tools="allowedToolsFor(orch.state.designerRunId)"
              :render-text="renderText"
              @decide="(d, r) => handleDecide(orch.state.designerRunId, d, r)"
              @answer="(a) => handleAnswer(orch.state.designerRunId, a)"
            />
          </div>
        </div>
      </div>

      <div class="flex min-h-0 flex-col overflow-hidden bg-background">
        <div class="flex items-center gap-2 border-b border-border px-3 py-2 shrink-0">
          <span class="text-xs font-semibold">{{ orch.state.reviewerLabel }}</span>
          <span class="text-[11px] text-muted-foreground font-mono">{{ orch.state.reviewerEngine }}</span>
          <div class="ml-auto flex items-center gap-1.5">
            <Loader2 v-if="isRunning('reviewer')" class="h-3.5 w-3.5 animate-spin text-primary" />
            <button v-if="orch.state.reviewerRunId"
              class="inline-flex items-center gap-1 h-6 px-1.5 rounded text-[11px] text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
              :title="t('duo.openSession')"
              @click="openSession(orch.state.reviewerRunId)"
            >
              <ExternalLink class="h-3.5 w-3.5" :stroke-width="2" />
              {{ t('duo.openSession') }}
            </button>
          </div>
        </div>
        <div class="relative flex-1 min-h-0">
          <div ref="reviewerScrollEl" class="absolute inset-0 overflow-y-auto px-3 py-2">
            <StreamLog v-if="reviewerEntries.length" :entries="reviewerEntries"
              :running="isRunning('reviewer')" :render-text="renderText" />
            <p v-else class="text-xs text-muted-foreground p-4 text-center">{{ t('duo.emptyReviewer') }}</p>
          </div>
          <div v-if="reviewerPermission"
            class="absolute inset-0 z-20 overflow-auto bg-card border-t border-border">
            <PermissionPrompt
              :key="reviewerPermission.request_id"
              :request="reviewerPermission"
              :allowed-tools="allowedToolsFor(orch.state.reviewerRunId)"
              :render-text="renderText"
              @decide="(d, r) => handleDecide(orch.state.reviewerRunId, d, r)"
              @answer="(a) => handleAnswer(orch.state.reviewerRunId, a)"
            />
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
