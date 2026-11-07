<script setup lang="ts">
/**
 * Create or edit one todo on the phone Controller — a full-height screen laid
 * out like the note editor: title, project, due date (with Today / Tomorrow /
 * Next week / No date chips) and a note. On a Host with the Focus board it
 * also edits the hand-set Focus fields — time, priority, estimate, status —
 * and the AI: a new todo takes a source link with ✨ Suggest (fills the form,
 * nothing saved until Save); an existing one shows its AI plan as Markdown,
 * ✨ Analyze (spinner + Cancel) with the analysis' estimate / priority /
 * deadline to Apply into the form (like the desktop's former AI panel), and
 * 🚀 Start with AI.
 *
 * Writes go through the `save` / `remove` props; the screen only goes back once
 * the Host accepted them, and stays (busy) meanwhile.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, ExternalLink, Loader2, Rocket, Sparkles, X } from 'lucide-vue-next'
import { AppSelect, Banner, Button, Input, Textarea } from '@/components/ui'
import {
  applySuggestion,
  emptyTodoDraft,
  formatEstimate,
  parseEstimate,
  type AiAnalysis,
  type AiSuggestion,
  type FocusTodo,
  type TodoPriority,
  type TodoStatus,
} from '@/lib/focus'
import { suggestAiKey, todoAiKey } from '@/lib/remoteFocus'
import { useFocusText } from '@/components/focus/useFocusText'
import { useControllerErrors } from '../focusErrors'
import {
  activeDueChip,
  DUE_CHIPS,
  dueFromChip,
  joinTodoText,
  splitTodoText,
  type DueChip,
} from '@/lib/remoteTodos'
import ControllerHeader from './ControllerHeader.vue'
import HeaderButton from './HeaderButton.vue'
import type { ProjectInfo, TodoInfo } from '../protocol'
import type { AiOutcome, TodoSave } from '../dataStore'

const props = defineProps<{
  /** The todo being edited; null for a new one. */
  todo: TodoInfo | null
  projects: ProjectInfo[]
  /** Project a new todo starts in ('' = none). */
  defaultProjectId: string
  /** Today as local `YYYY-MM-DD`. */
  today: string
  busy: boolean
  error: string | null
  save: (input: TodoSave) => Promise<boolean>
  remove: (id: string) => Promise<boolean>
  /** The Host ranks todos: show the Focus fields. */
  focus?: boolean
  /** The todo's Focus fields (from the board); null for a new todo or one the
   * board does not carry (done long ago). */
  focusTodo?: FocusTodo | null
  /** The same row as the board has it now (its AI plan follows an analysis). */
  liveTodo?: FocusTodo | null
  /** Markdown → HTML (the controller's renderer). */
  renderText?: (text: string) => string
  /** Running AI requests / last failures by key, analyses by todo id. */
  aiRunning?: Record<string, string>
  aiErrors?: Record<string, string>
  analyses?: Record<string, AiAnalysis>
  /** ✨ Suggest from a source link. */
  suggest?: (sourceUrl: string, projectId: string | null, title: string | null) => Promise<AiOutcome<AiSuggestion>>
}>()

const emit = defineEmits<{
  back: []
  'clear-error': []
  analyze: [id: string]
  'cancel-ai': [key: string]
  'start-ai': [todo: FocusTodo]
}>()

const { t } = useI18n()
const { stamp } = useFocusText()
const errors = useControllerErrors()

const title = ref('')
const note = ref('')
const dueDate = ref('')
const projectId = ref('')
const dueTime = ref('')
const priority = ref<TodoPriority>(2)
const estimate = ref('')
const status = ref<TodoStatus>('todo')
const sourceUrl = ref('')
/** Minutes last applied from an AI analysis (saved as `estimate_source: 'ai'`). */
const aiEstimateMin = ref<number | null>(null)
const suggestNote = ref<string | null>(null)
const confirmingDelete = ref(false)
const localError = ref<string | null>(null)

watch(
  () => props.todo,
  (todo) => {
    const parts = splitTodoText(todo?.text ?? '')
    title.value = parts.title
    note.value = parts.note
    dueDate.value = todo?.due_date ?? ''
    projectId.value = todo ? (todo.project_id ?? '') : props.defaultProjectId
    const f = props.focusTodo
    dueTime.value = f?.due_time ?? todo?.due_time ?? ''
    priority.value = f?.priority ?? 2
    estimate.value = f?.estimate_min ? formatEstimate(f.estimate_min) : ''
    status.value = f?.status ?? 'todo'
    sourceUrl.value = ''
    aiEstimateMin.value = null
    suggestNote.value = null
    confirmingDelete.value = false
    localError.value = null
  },
  { immediate: true },
)

const projectOptions = computed(() => [
  { value: '', label: t('controller.data.noProject') },
  ...props.projects.map((p) => ({ value: p.id, label: p.name })),
])

const activeChip = computed(() => activeDueChip(dueDate.value, props.today))

function pickChip(chip: DueChip): void {
  dueDate.value = dueFromChip(chip, props.today)
}

// Focus fields are editable when the Host ranks todos and either this is a new
// todo or the board carries this one (a todo done long ago is not on it).
const showFocus = computed(() => !!props.focus && (!props.todo || !!props.focusTodo))
const PRIORITIES: TodoPriority[] = [1, 2, 3]
const STATUSES: TodoStatus[] = ['todo', 'doing', 'waiting']
const ESTIMATE_CHIPS = [15, 30, 60, 120, 240]
const estimateMin = computed(() => parseEstimate(estimate.value))
/** The source as a link the phone can open (http(s) only — `run:` ids and
 * `owner/repo#12` shorthands would resolve against the relay's origin). */
const sourceHref = computed(() => {
  const url = props.focusTodo?.source_url ?? ''
  return /^https?:\/\//i.test(url) ? url : null
})

// ── AI ──
const aiRunning = computed(() => props.aiRunning ?? {})
const live = computed(() => props.liveTodo ?? props.focusTodo ?? null)
const analyzeKey = computed(() => (props.todo ? todoAiKey(props.todo.id) : ''))
const analyzing = computed(() => !!analyzeKey.value && !!aiRunning.value[analyzeKey.value])
const analysis = computed(() => (props.todo ? (props.analyses?.[props.todo.id] ?? null) : null))
const analyzeError = computed(() => errors.aiError(props.aiErrors?.[analyzeKey.value]))
const planHtml = computed(() => {
  const plan = live.value?.ai_plan
  if (!plan) return ''
  return props.renderText ? props.renderText(plan) : ''
})

const estimateText = computed(() => {
  const a = analysis.value
  if (!a) return ''
  return t('focus.ai.estimate', {
    low: formatEstimate(a.estimate_low_min),
    high: formatEstimate(a.estimate_high_min),
    likely: formatEstimate(a.estimate_min),
  })
})
const suggestedPriority = computed(() => {
  const p = analysis.value?.suggested_priority
  return p && p !== priority.value ? p : null
})
const suggestedDue = computed(() => {
  const a = analysis.value
  if (!a?.suggested_due_date) return null
  if (a.suggested_due_date === dueDate.value && (a.suggested_due_time ?? '') === dueTime.value) return null
  return { date: a.suggested_due_date, time: a.suggested_due_time }
})
function applyEstimate(min: number): void {
  estimate.value = formatEstimate(min)
  aiEstimateMin.value = min
}
function applyDue(date: string, time: string | null): void {
  dueDate.value = date
  dueTime.value = time ?? ''
}

const suggestKey = computed(() => suggestAiKey(sourceUrl.value))
const suggesting = computed(() => !!sourceUrl.value.trim() && !!aiRunning.value[suggestKey.value])
async function onSuggest(): Promise<void> {
  const url = sourceUrl.value.trim()
  if (!url || !props.suggest || suggesting.value) return
  suggestNote.value = null
  localError.value = null
  const out = await props.suggest(url, projectId.value || null, title.value.trim() || null)
  if (!out.ok) {
    localError.value = errors.aiError(out.code)
    return
  }
  // The user may have typed meanwhile: apply onto the current form.
  const next = applySuggestion(
    emptyTodoDraft({
      text: title.value,
      projectId: projectId.value,
      dueDate: dueDate.value,
      dueTime: dueTime.value,
      priority: priority.value,
      estimate: estimate.value,
      sourceUrl: url,
    }),
    out.result,
  )
  title.value = next.text
  dueDate.value = next.dueDate
  dueTime.value = next.dueTime
  priority.value = next.priority
  estimate.value = next.estimate
  suggestNote.value = out.result.reason
    ? t('focus.ai.suggestedWhy', { reason: out.result.reason })
    : t('focus.ai.suggested')
}

function onStartAi(): void {
  const f = live.value
  if (!f) return
  // The session runs in the todo's project: it needs one (saved) first.
  if (!f.project_id) {
    localError.value = t('focus.ai.errors.project_required')
    return
  }
  emit('start-ai', f)
}

async function submit(): Promise<void> {
  if (props.busy) return
  if (!title.value.trim()) {
    localError.value = t('controller.todo.sheet.needsTitle')
    return
  }
  if (showFocus.value && estimateMin.value === undefined) {
    localError.value = t('focus.drawer.estimateInvalid')
    return
  }
  localError.value = null
  const ok = await props.save({
    id: props.todo?.id ?? null,
    text: joinTodoText(title.value, note.value),
    dueDate: dueDate.value,
    projectId: projectId.value,
    base: props.todo,
    focus: showFocus.value
      ? {
          dueTime: dueTime.value,
          priority: priority.value,
          estimateMin: estimateMin.value ?? null,
          status: status.value,
          base: props.focusTodo ?? null,
          aiEstimateMin: aiEstimateMin.value,
          sourceUrl: props.todo ? undefined : sourceUrl.value,
        }
      : undefined,
  })
  if (ok) emit('back')
}

/** Two taps instead of a modal. */
async function onDelete(): Promise<void> {
  if (!props.todo || props.busy) return
  if (!confirmingDelete.value) {
    confirmingDelete.value = true
    return
  }
  if (await props.remove(props.todo.id)) emit('back')
}

const banner = computed(() => localError.value ?? props.error)
function clearBanner(): void {
  localError.value = null
  emit('clear-error')
}
</script>

<template>
  <div class="app-shell relative flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="todo ? t('controller.todo.sheet.editTitle') : t('controller.todo.sheet.newTitle')">
      <template #leading>
        <HeaderButton class="-ml-1.5" :label="t('controller.data.back')" @click="emit('back')">
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </HeaderButton>
      </template>
    </ControllerHeader>

    <Banner
      v-if="banner"
      tone="destructive"
      class="shrink-0 gap-2 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{ banner }}
      <template #actions>
        <button
          class="shrink-0 cursor-pointer text-destructive"
          :title="t('controller.data.dismiss')"
          :aria-label="t('controller.data.dismiss')"
          @click="clearBanner"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </template>
    </Banner>

    <main class="flex-1 min-h-0 overflow-auto px-3 py-3">
      <form class="flex min-h-full flex-col" @submit.prevent="submit">
        <label class="mb-3 block">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.title') }}</span>
          <Input
            v-model="title"
            size="md"
            class="w-full"
            autocomplete="off"
            enterkeyhint="done"
            :placeholder="t('controller.todo.sheet.titlePlaceholder')"
          />
        </label>

        <div class="mb-3">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.data.project') }}</span>
          <AppSelect v-model="projectId" size="md" class="w-full" :options="projectOptions" />
        </div>

        <div v-if="showFocus && !todo" class="mb-3">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('focus.drawer.source') }}</span>
          <div class="flex items-center gap-2">
            <Input
              v-model="sourceUrl"
              type="url"
              size="md"
              class="min-w-0 flex-1"
              autocomplete="off"
              inputmode="url"
              :placeholder="t('controller.focus.editor.sourcePlaceholder')"
            />
            <template v-if="sourceUrl.trim()">
              <Button v-if="suggesting" variant="ghost" size="md" @click="emit('cancel-ai', suggestKey)">
                <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
                {{ t('focus.ai.cancel') }}
              </Button>
              <Button v-else-if="suggest" variant="outline" size="md" :title="t('focus.ai.suggestHint')" @click="onSuggest">
                <Sparkles class="h-3.5 w-3.5" :stroke-width="2" />
                {{ t('focus.capture.suggest') }}
              </Button>
            </template>
          </div>
          <p v-if="suggesting" class="mt-1 text-[11px] text-muted-foreground">{{ t('focus.ai.suggesting') }}</p>
          <p v-else-if="suggestNote" class="mt-1 text-[11px] text-muted-foreground">{{ suggestNote }}</p>
        </div>

        <label class="mb-2 block">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.due') }}</span>
          <Input v-model="dueDate" type="date" size="md" class="w-full" />
        </label>
        <div class="mb-3 flex flex-wrap gap-1.5">
          <button
            v-for="chip in DUE_CHIPS"
            :key="chip"
            type="button"
            class="rounded-full border px-2.5 py-1 text-xs transition-colors cursor-pointer"
            :class="
              activeChip === chip
                ? 'border-primary bg-primary/15 text-foreground'
                : 'border-border bg-secondary text-muted-foreground hover:text-foreground'
            "
            @click="pickChip(chip)"
          >
            {{ t(`controller.todo.chips.${chip}`) }}
          </button>
        </div>

        <template v-if="showFocus">
          <label v-if="dueDate" class="mb-3 block">
            <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('focus.drawer.dueTime') }}</span>
            <Input v-model="dueTime" type="time" size="md" class="w-full" />
          </label>

          <div class="mb-3">
            <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('focus.priority.label') }}</span>
            <div class="ui-segmented flex gap-1 rounded-lg border border-border bg-card p-0.5">
              <button
                v-for="p in PRIORITIES"
                :key="p"
                type="button"
                class="flex-1 rounded-md px-2 py-1.5 text-xs transition-colors cursor-pointer"
                :class="priority === p ? 'is-active bg-secondary font-semibold text-foreground' : 'text-muted-foreground'"
                @click="priority = p"
              >
                {{ t(`focus.priority.${p}`) }}
              </button>
            </div>
          </div>

          <label class="mb-2 block">
            <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('focus.drawer.estimate') }}</span>
            <Input
              v-model="estimate"
              size="md"
              class="w-full"
              autocomplete="off"
              inputmode="text"
              :placeholder="t('focus.drawer.estimatePlaceholder')"
            />
          </label>
          <div class="mb-3 flex flex-wrap gap-1.5">
            <button
              v-for="m in ESTIMATE_CHIPS"
              :key="m"
              type="button"
              class="rounded-full border px-2.5 py-1 text-xs transition-colors cursor-pointer"
              :class="
                estimateMin === m
                  ? 'border-primary bg-primary/15 text-foreground'
                  : 'border-border bg-secondary text-muted-foreground hover:text-foreground'
              "
              @click="estimate = formatEstimate(m)"
            >
              {{ formatEstimate(m) }}
            </button>
          </div>

          <div class="mb-3">
            <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('focus.status.label') }}</span>
            <div class="ui-segmented flex gap-1 rounded-lg border border-border bg-card p-0.5">
              <button
                v-for="st in STATUSES"
                :key="st"
                type="button"
                class="flex-1 rounded-md px-2 py-1.5 text-xs transition-colors cursor-pointer"
                :class="status === st ? 'is-active bg-secondary font-semibold text-foreground' : 'text-muted-foreground'"
                @click="status = st"
              >
                {{ t(`focus.status.${st}`) }}
              </button>
            </div>
          </div>

          <a
            v-if="sourceHref && focusTodo"
            :href="sourceHref"
            target="_blank"
            rel="noopener noreferrer"
            class="mb-3 flex items-center gap-1.5 text-xs text-primary"
          >
            <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
            <span class="truncate">{{ t('focus.actions.openSource') }} · {{ focusTodo.source_ref ?? focusTodo.source_url }}</span>
          </a>
          <p v-else-if="focusTodo?.source_ref" class="mb-3 truncate text-xs text-muted-foreground">
            {{ t('focus.drawer.source') }} · {{ focusTodo.source_ref }}
          </p>
        </template>

        <label class="mb-3 flex flex-1 flex-col">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.note') }}</span>
          <Textarea
            v-model="note"
            size="md"
            class="min-h-[10rem] w-full flex-1 resize-none"
            :placeholder="t('controller.todo.sheet.notePlaceholder')"
          />
        </label>

        <section v-if="showFocus && todo && live" class="mb-3 rounded-xl border border-border bg-card px-3 py-2.5">
          <div class="flex flex-wrap items-center gap-2">
            <h3 class="mr-auto text-[11px] font-medium text-muted-foreground">
              {{ t('focus.drawer.aiPlan') }}
              <span v-if="live.ai_analyzed_at"> · {{ t('focus.drawer.aiAnalyzedAt', { time: stamp(live.ai_analyzed_at) }) }}</span>
            </h3>
          </div>
          <div class="mt-2 flex flex-wrap items-center gap-2">
            <template v-if="analyzing">
              <span class="flex items-center gap-1 text-xs text-muted-foreground">
                <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
                {{ t('focus.ai.analyzing') }}
              </span>
              <Button variant="ghost" size="sm" @click="emit('cancel-ai', analyzeKey)">
                <X class="h-3 w-3" :stroke-width="2" />
                {{ t('focus.ai.cancel') }}
              </Button>
            </template>
            <Button v-else variant="outline" size="sm" :disabled="todo.done" @click="emit('analyze', todo.id)">
              <Sparkles class="h-3.5 w-3.5" :stroke-width="2" />
              {{ live.ai_plan ? t('focus.ai.reanalyze') : t('focus.ai.analyze') }}
            </Button>
            <Button variant="outline" size="sm" :disabled="todo.done || busy" @click="onStartAi">
              <Rocket class="h-3.5 w-3.5" :stroke-width="2" />
              {{ t('focus.ai.start') }}
            </Button>
          </div>
          <p v-if="analyzeError && !analyzing" class="mt-2 text-xs text-destructive">{{ analyzeError }}</p>

          <!-- What the last analysis proposes (applied into the form, saved with it) -->
          <div v-if="analysis" class="mt-2 space-y-1 text-xs">
            <p class="flex flex-wrap items-center gap-2">
              <span>{{ estimateText }}</span>
              <button type="button" class="cursor-pointer font-medium text-primary" @click="applyEstimate(analysis.estimate_min)">
                {{ t('focus.ai.apply') }}
              </button>
            </p>
            <p v-if="suggestedPriority" class="flex flex-wrap items-center gap-2">
              <span>{{ t('focus.ai.suggestedPriority', { p: suggestedPriority }) }}</span>
              <button type="button" class="cursor-pointer font-medium text-primary" @click="priority = suggestedPriority">
                {{ t('focus.ai.apply') }}
              </button>
            </p>
            <p v-if="suggestedDue" class="flex flex-wrap items-center gap-2">
              <span>
                {{ t('focus.ai.suggestedDue', { due: suggestedDue.time ? `${suggestedDue.date} ${suggestedDue.time}` : suggestedDue.date }) }}
              </span>
              <button
                type="button"
                class="cursor-pointer font-medium text-primary"
                @click="applyDue(suggestedDue.date, suggestedDue.time)"
              >
                {{ t('focus.ai.apply') }}
              </button>
            </p>
          </div>

          <div v-if="planHtml" class="markdown-output mt-2 max-h-80 overflow-auto text-sm" v-html="planHtml" />
          <p v-else-if="live.ai_plan" class="mt-2 max-h-60 overflow-auto whitespace-pre-wrap text-xs">{{ live.ai_plan }}</p>
          <p v-else-if="!analyzing" class="mt-2 text-[11px] text-muted-foreground">{{ t('focus.ai.noPlan') }}</p>
        </section>

        <div class="grid grid-cols-2 gap-2 pb-[env(safe-area-inset-bottom)]">
          <Button v-if="todo" variant="destructive" size="md" :disabled="busy" @click="onDelete">
            {{ confirmingDelete ? t('controller.todo.sheet.confirmDelete') : t('controller.todo.sheet.delete') }}
          </Button>
          <Button v-else variant="outline" size="md" :disabled="busy" @click="emit('back')">
            {{ t('controller.data.cancel') }}
          </Button>
          <Button type="submit" size="md" :disabled="busy">
            {{ busy ? t('controller.data.saving') : t('controller.data.save') }}
          </Button>
        </div>
      </form>
    </main>
  </div>
</template>
