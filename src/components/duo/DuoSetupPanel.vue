<script setup lang="ts">
/**
 * Duo config form — everything needed to start (or restart) a duo: the two
 * sides' engines/models/roles, the goal, and the stop conditions.
 *
 * It owns its own form state and calls `orch.start()` itself, then emits
 * `started` so the workspace can collapse it and hand the screen over to the
 * conversation. Restoring a saved duo (from the history list) flows in through
 * watchers on the shared store, so the form re-syncs no matter who triggered it.
 *
 * `projectId` prop: when given, the duo is locked to that project (the project
 * selector is hidden) — used when embedded in the project screen.
 */
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Play, RotateCcw, Loader2 } from 'lucide-vue-next'
import { useProjectsStore } from '@/stores/projects'
import { useRunsStore, runActivityAt } from '@/stores/runs'
import { useOrchestratorStore, type DuoSourceMode } from '@/stores/orchestrator'
import { useDuoDraftsStore, type DuoDraft } from '@/stores/duoDrafts'
import { effectiveModelOptions, PERMISSION_MODE_OPTIONS } from '@/lib/engineOptions'
import { useModelCatalogStore } from '@/stores/modelCatalog'
import { useToast } from '@/composables/useToast'
import { Button, AppSelect, Input, Textarea } from '@/components/ui'

const props = defineProps<{ projectId?: string }>()
const emit = defineEmits<{ started: [] }>()

const { t } = useI18n()
const projectsStore = useProjectsStore()
const runsStore = useRunsStore()
const orch = useOrchestratorStore()
const duoDrafts = useDuoDraftsStore()
const modelCatalog = useModelCatalogStore()
const { toast } = useToast()

const ENGINE_OPTIONS = [
  { value: 'claude', label: 'claude' },
  { value: 'codex', label: 'codex' },
]

const lockedProject = computed(() => !!props.projectId)

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

const projectOptions = computed(() =>
  projectsStore.projects.map((p) => ({ value: p.id, label: p.name })),
)
// Same source as Settings and the Run composer: the curated Claude table
// (annotated with the last validation sweep) and the cached Codex catalog.
const discoveredModels = computed(() => ({ codex: modelCatalog.codexModels }))
const designerModelOptions = computed(() =>
  modelCatalog.withValidation(
    effectiveModelOptions(designerEngine.value, discoveredModels.value),
  ),
)
const reviewerModelOptions = computed(() =>
  modelCatalog.withValidation(
    effectiveModelOptions(reviewerEngine.value, discoveredModels.value),
  ),
)

const isActive = computed(() => orch.state.active)

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
    const when = formatWhen(runActivityAt(r))
    const parts = [r.engine, r.status, when, `#${r.id.slice(0, 6)}`].filter(Boolean)
    return { value: r.id, label: `${r.pinned ? '📌 ' : ''}${label}`, description: parts.join(' · ') }
  })
})

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

watch([projectId, sourceMode], (cur, prev) => {
  if (hydrating || restoringDraft) return
  existingDesignerId.value = ''
  existingReviewerId.value = ''
  if (sourceMode.value === 'existing') void loadProjectRuns()
  // Switching project (standalone route) loads that project's saved draft.
  if (cur[0] !== prev[0]) applyDraft()
})

// Bumping the cap on a paused duo and hitting Continue should extend it, so
// mirror the field into the store while it isn't running.
watch(maxRounds, (v) => {
  if (hydrating || orch.state.active || orch.state.phase === 'idle') return
  if (Number.isFinite(v) && v > 0) orch.state.maxRounds = v
})

// ── Idle-form draft persistence ──────────────────────────────────────────
// The workspace mounts this panel on demand, so a half-composed "new duo" would
// vanish on a tab switch or reload. Cache the idle form per project and restore
// it when the panel comes back. `restoringDraft` starts true so the persist
// watcher stays quiet until onMounted has finished seeding the form.
let restoringDraft = true

function draftKey(): string {
  return projectId.value || '__global__'
}

/** True only while a fresh "new duo" is being composed (no started/restored
 * duo drives the form), i.e. when a draft is meaningful to keep. */
function draftEditable(): boolean {
  return !isActive.value && orch.state.phase === 'idle'
}

function applyDraft() {
  if (!draftEditable()) return
  const d = duoDrafts.get(draftKey())
  if (!d) return
  restoringDraft = true
  sourceMode.value = d.sourceMode
  existingDesignerId.value = d.existingDesignerId
  existingReviewerId.value = d.existingReviewerId
  designerEngine.value = d.designerEngine
  reviewerEngine.value = d.reviewerEngine
  designerModel.value = d.designerModel
  reviewerModel.value = d.reviewerModel
  designerLabel.value = d.designerLabel
  reviewerLabel.value = d.reviewerLabel
  designerInstruction.value = d.designerInstruction
  reviewerInstruction.value = d.reviewerInstruction
  permissionMode.value = d.permissionMode
  goal.value = d.goal
  maxRounds.value = d.maxRounds
  consensusToken.value = d.consensusToken
  overrideBudget.value = d.overrideBudget
  if (d.sourceMode === 'existing') void loadProjectRuns()
  nextTick(() => { restoringDraft = false })
}

function captureDraft(): DuoDraft {
  return {
    sourceMode: sourceMode.value,
    existingDesignerId: existingDesignerId.value,
    existingReviewerId: existingReviewerId.value,
    designerEngine: designerEngine.value,
    reviewerEngine: reviewerEngine.value,
    designerModel: designerModel.value,
    reviewerModel: reviewerModel.value,
    designerLabel: designerLabel.value,
    reviewerLabel: reviewerLabel.value,
    designerInstruction: designerInstruction.value,
    reviewerInstruction: reviewerInstruction.value,
    permissionMode: permissionMode.value,
    goal: goal.value,
    maxRounds: maxRounds.value,
    consensusToken: consensusToken.value,
    overrideBudget: overrideBudget.value,
  }
}

// Mirror every idle-form edit to the draft store. Hydration / draft-restore
// and any active/restored duo are skipped so we only ever cache genuine edits.
watch(
  [
    sourceMode, existingDesignerId, existingReviewerId, designerEngine, reviewerEngine,
    designerModel, reviewerModel, designerLabel, reviewerLabel, designerInstruction,
    reviewerInstruction, permissionMode, goal, maxRounds, consensusToken, overrideBudget,
  ],
  () => {
    if (restoringDraft || hydrating || !draftEditable()) return
    duoDrafts.set(draftKey(), captureDraft())
  },
)

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
  nextTick(() => {
    hydrating = false
    if (s.mode === 'existing') void loadProjectRuns()
  })
}

/** Reset the config form back to editable defaults. */
function resetForm() {
  // Silence the persist watcher so these defaults don't re-save as a draft, and
  // drop any cached draft so "New" truly starts clean.
  restoringDraft = true
  duoDrafts.clear(draftKey())
  nextTick(() => { restoringDraft = false })
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
  if (lockedProject.value) projectId.value = props.projectId ?? ''
}

// The history list drives restore/new via the shared store; react to it here so
// the form stays in sync no matter which component triggered it.
watch(() => orch.state.id, () => hydrateFromState())
watch(() => orch.draftNonce, () => resetForm())
watch(() => props.projectId, (id) => { if (id && !isActive.value) projectId.value = id })

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
    // The config now lives in the orchestrator history; drop the idle draft so a
    // later "new duo" doesn't reopen this one's leftovers.
    duoDrafts.clear(draftKey())
    emit('started')
  } catch (e) {
    toast.error(t('duo.startFailed', { error: String(e) }))
  } finally {
    starting.value = false
  }
}

function newDuo() {
  if (isActive.value) return
  orch.newDraft()
}

function onDesignerEngineChange(v: string) {
  designerEngine.value = v
  if (!designerModelOptions.value.some((o) => o.value === designerModel.value)) {
    designerModel.value = ''
  }
}
function onReviewerEngineChange(v: string) {
  reviewerEngine.value = v
  if (!reviewerModelOptions.value.some((o) => o.value === reviewerModel.value)) {
    reviewerModel.value = ''
  }
}

onMounted(async () => {
  // Reads the persisted cache only — never triggers discovery.
  modelCatalog.ensureLoaded().catch(() => {})
  hydrateFromState()
  if (!designerLabel.value) designerLabel.value = t('duo.defaults.designerLabel')
  if (!reviewerLabel.value) reviewerLabel.value = t('duo.defaults.reviewerLabel')
  if (!designerInstruction.value) designerInstruction.value = t('duo.defaults.designerInstruction')
  if (!reviewerInstruction.value) reviewerInstruction.value = t('duo.defaults.reviewerInstruction')
  if (!projectsStore.projects.length) await projectsStore.fetchProjects().catch(() => {})
  if (!projectId.value && !lockedProject.value && projectsStore.projects.length) {
    projectId.value = projectsStore.projects[0].id
  }
  // Restore any half-composed idle form for this project, then release the
  // persist watcher so subsequent edits (and only those) are cached.
  applyDraft()
  await nextTick()
  restoringDraft = false
})
</script>

<template>
  <section class="border-b border-border bg-card/40 px-4 py-3 space-y-3 shrink-0">
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
      <Button v-if="!isActive && (orch.state.turns.length || orch.state.phase !== 'idle')" variant="ghost" @click="newDuo">
        <RotateCcw class="h-3.5 w-3.5" :stroke-width="2" />
        {{ t('duo.action.reset') }}
      </Button>

      <label class="flex items-center gap-1.5 text-xs text-muted-foreground ml-auto">
        <input v-model="overrideBudget" type="checkbox" :disabled="isActive" class="accent-primary" />
        {{ t('duo.field.overrideBudget') }}
      </label>
    </div>
  </section>
</template>
