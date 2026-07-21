<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { ShieldAlert, Check, X, FileEdit, HelpCircle, ChevronLeft, ChevronRight, ClipboardList } from 'lucide-vue-next'
import { Button } from '@/components/ui'

export interface PermissionRequest {
  run_id: string
  request_id: string
  tool_name: string
  tool_input: unknown
  session_id?: string | null
  cwd?: string | null
  /** Bridge-rendered prompt sentence (e.g. "Claude wants to edit foo.ts"). */
  title?: string | null
  /** Bridge-rendered subtitle describing the effect. */
  description?: string | null
  /** Short noun phrase for the action (e.g. "Edit file"). */
  display_name?: string | null
}

const props = defineProps<{
  request: PermissionRequest
  /** Names of tools the user already chose "allow always" for, this session. */
  allowedTools?: string[]
  /** Markdown → HTML renderer (used to show the plan for ExitPlanMode). */
  renderText?: (md: string) => string
}>()

const emit = defineEmits<{
  decide: [decision: 'allow' | 'deny', remember: boolean]
  answer: [answers: Record<string, string>]
}>()

function asObject(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}

const input = computed(() => asObject(props.request.tool_input))

const filePath = computed(() => {
  const p = input.value['file_path'] ?? input.value['path']
  return typeof p === 'string' ? p : ''
})

/** Edit/Write/MultiEdit produce a diff we can show inline, like the extension. */
interface DiffPart {
  before: string
  after: string
}
const diffParts = computed<DiffPart[]>(() => {
  const name = props.request.tool_name
  const inp = input.value
  if (name === 'Edit') {
    const before = typeof inp['old_string'] === 'string' ? (inp['old_string'] as string) : ''
    const after = typeof inp['new_string'] === 'string' ? (inp['new_string'] as string) : ''
    if (before || after) return [{ before, after }]
  }
  if (name === 'MultiEdit' && Array.isArray(inp['edits'])) {
    return (inp['edits'] as unknown[])
      .map((e) => asObject(e))
      .map((e) => ({
        before: typeof e['old_string'] === 'string' ? (e['old_string'] as string) : '',
        after: typeof e['new_string'] === 'string' ? (e['new_string'] as string) : '',
      }))
      .filter((d) => d.before || d.after)
  }
  if (name === 'Write') {
    const after = typeof inp['content'] === 'string' ? (inp['content'] as string) : ''
    if (after) return [{ before: '', after }]
  }
  return []
})

const hasDiff = computed(() => diffParts.value.length > 0)

/** AskUserQuestion: render the structured questions as selectable options. */
const isQuestions = computed(() => props.request.tool_name === 'AskUserQuestion')

// ExitPlanMode isn't a risky side-effecting tool — it's Claude presenting a plan
// and asking to leave plan mode. Render it as a plan review (Approve / Keep
// planning) instead of the generic "allow this tool call?" permission prompt.
const isPlan = computed(() => props.request.tool_name === 'ExitPlanMode')
const canRemember = computed(() => !isPlan.value && props.request.tool_name !== 'MCP')
const planText = computed(() => {
  const p = input.value['plan']
  return typeof p === 'string' ? p : ''
})
const planHtml = computed(() =>
  props.renderText && planText.value ? props.renderText(planText.value) : '',
)

interface QuestionOption {
  label: string
  description: string
  preview?: string
}
interface Question {
  question: string
  header: string
  multiSelect: boolean
  options: QuestionOption[]
}

const questions = computed<Question[]>(() => {
  if (!isQuestions.value) return []
  const raw = input.value['questions']
  if (!Array.isArray(raw)) return []
  return raw.map((q) => {
    const o = asObject(q)
    const opts = Array.isArray(o['options'])
      ? (o['options'] as unknown[])
          .map((x) => {
            const oo = asObject(x)
            return {
              label: typeof oo['label'] === 'string' ? (oo['label'] as string) : '',
              description: typeof oo['description'] === 'string' ? (oo['description'] as string) : '',
              preview: typeof oo['preview'] === 'string' ? (oo['preview'] as string) : undefined,
            }
          })
          .filter((x) => x.label)
      : []
    return {
      question: typeof o['question'] === 'string' ? (o['question'] as string) : '',
      header: typeof o['header'] === 'string' ? (o['header'] as string) : '',
      multiSelect: o['multiSelect'] === true,
      options: opts,
    }
  })
})

// Per-question selected option labels + freeform "Other" text, indexed by question.
const selected = ref<string[][]>([])
const other = ref<string[]>([])
// Questions are shown one at a time (a stepper): a long option list scrolls under
// a single pinned question header instead of many stacked sticky headers piling
// up and covering each other. `step` is the active question; `focusedIndex` is
// the keyboard cursor within that question's items (options + the Other input).
const step = ref(0)
const focusedIndex = ref(0)

watch(
  questions,
  (qs) => {
    selected.value = qs.map(() => [])
    other.value = qs.map(() => '')
    step.value = 0
    focusedIndex.value = 0
  },
  { immediate: true },
)

const currentQuestion = computed<Question | undefined>(() => questions.value[step.value])
const stepCount = computed(() => questions.value.length)
const isLastStep = computed(() => step.value >= stepCount.value - 1)
const otherIndex = computed(() => currentQuestion.value?.options.length ?? 0)

function isSelected(label: string): boolean {
  return (selected.value[step.value] ?? []).includes(label)
}

// --- Keyboard focus model (scoped to the current question) -----------------
// Items = the current question's options (index 0..n-1) followed by its single
// "Other" input (index n). ArrowUp/Down walk them, Space toggles, Enter advances.
function isFocused(oi: number): boolean {
  return focusedIndex.value === oi && oi < otherIndex.value
}
function isOtherFocused(): boolean {
  return focusedIndex.value === otherIndex.value
}
// After moving focus: scroll into view; the "Other" stop grabs DOM focus so the
// user can type, otherwise blur any input and return focus to the panel so Space
// toggles options again.
function applyFocus() {
  nextTick(() => {
    const el = rootEl.value?.querySelector<HTMLElement>(`[data-flat="${focusedIndex.value}"]`)
    el?.scrollIntoView({ block: 'nearest' })
    if (isOtherFocused()) {
      el?.focus()
    } else {
      const active = document.activeElement as HTMLElement | null
      if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) {
        active.blur()
        rootEl.value?.focus()
      }
    }
  })
}
function moveFocus(delta: number) {
  const len = otherIndex.value + 1 // options + the Other input
  focusedIndex.value = Math.min(len - 1, Math.max(0, focusedIndex.value + delta))
  applyFocus()
}
function toggleFocused() {
  if (focusedIndex.value < otherIndex.value) {
    toggleOption(currentQuestion.value?.options[focusedIndex.value]?.label ?? '')
  }
}

function toggleOption(label: string) {
  const qi = step.value
  const q = currentQuestion.value
  if (!q) return
  // Keep keyboard focus in sync when the user clicks an option.
  const oi = q.options.findIndex((o) => o.label === label)
  if (oi >= 0) focusedIndex.value = oi
  const cur = selected.value[qi] ?? []
  if (q.multiSelect) {
    selected.value[qi] = cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label]
  } else {
    selected.value[qi] = cur.includes(label) ? [] : [label]
  }
}

// --- Stepper navigation ----------------------------------------------------
function goToStep(i: number) {
  const next = Math.min(stepCount.value - 1, Math.max(0, i))
  if (next === step.value) return
  step.value = next
  focusedIndex.value = 0
  applyFocus()
}
function goPrev() {
  goToStep(step.value - 1)
}
function goNext() {
  if (isLastStep.value) submitAnswers()
  else if (currentAnswered.value) goToStep(step.value + 1)
}

/** Combined answer for a question: selected labels plus any "Other" text. */
function answerFor(qi: number): string {
  const parts = [...(selected.value[qi] ?? [])]
  const free = (other.value[qi] ?? '').trim()
  if (free) parts.push(free)
  return parts.join(', ')
}

const currentAnswered = computed(() => answerFor(step.value).length > 0)

const canSubmit = computed(
  () => questions.value.length > 0 && questions.value.every((_, i) => answerFor(i).length > 0),
)

function submitAnswers() {
  if (!canSubmit.value) return
  const answers: Record<string, string> = {}
  questions.value.forEach((q, i) => {
    answers[q.question] = answerFor(i)
  })
  emit('answer', answers)
}

const headerTitle = computed(() => {
  if (isPlan.value) return `Claude finished planning`
  return props.request.title || (isQuestions.value ? `Claude has a question` : `Claude requests permission`)
})

const commandPreview = computed(() => {
  const inp = input.value
  for (const k of ['command', 'url', 'pattern', 'query']) {
    if (typeof inp[k] === 'string') return inp[k] as string
  }
  return ''
})

const inputPreview = computed(() => {
  const inp = props.request.tool_input
  if (inp == null) return ''
  if (typeof inp === 'string') return inp
  try {
    return JSON.stringify(inp, null, 2)
  } catch {
    return String(inp)
  }
})

function splitLines(s: string): string[] {
  return s.length ? s.split('\n') : []
}

// --- Hotkeys ---------------------------------------------------------------
// The panel grabs focus when it appears so its keydown handler receives keys
// without stealing them from the composer when the user clicks back into it.
const rootEl = ref<HTMLElement | null>(null)
onMounted(() => {
  rootEl.value?.focus()
})

function isEditableTarget(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null
  if (!t) return false
  return t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return // mid-IME composition
  const editable = isEditableTarget(e)

  if (isQuestions.value) {
    switch (e.key) {
      case 'Enter':
        e.preventDefault()
        goNext() // advance to the next question, or submit on the last one
        break
      case 'Escape':
        e.preventDefault()
        emit('decide', 'deny', false)
        break
      // ArrowUp/Down also work from the single-line "Other" input (where they'd
      // otherwise be no-ops), so the keyboard can move into and back out of it.
      case 'ArrowDown':
        e.preventDefault()
        moveFocus(1)
        break
      case 'ArrowUp':
        e.preventDefault()
        moveFocus(-1)
        break
      case 'ArrowRight':
        if (editable) return // let the cursor move within the "Other" field
        e.preventDefault()
        goToStep(step.value + 1)
        break
      case 'ArrowLeft':
        if (editable) return
        e.preventDefault()
        goToStep(step.value - 1)
        break
      case ' ':
      case 'Spacebar':
        if (editable) return // typing a space in "Other"
        e.preventDefault() // also stops the panel from scrolling
        toggleFocused()
        break
    }
    return
  }

  // Permission mode: Enter = allow once, Shift+Enter = allow always, Esc = deny.
  // Plan mode (ExitPlanMode): Enter = approve (never auto-allow — a plan review
  // isn't a tool to remember), Esc = keep planning.
  switch (e.key) {
    case 'Enter':
      e.preventDefault()
      emit('decide', 'allow', canRemember.value && e.shiftKey)
      break
    case 'Escape':
      e.preventDefault()
      emit('decide', 'deny', false)
      break
  }
}
</script>

<template>
  <!-- Fills its host container (a right-side drawer in the run view, or the
       pop-out window), so the user can still read chat history / switch sessions
       while a permission request or question is pending. -->
  <div
    ref="rootEl"
    tabindex="-1"
    class="prompt-attention flex h-full min-h-0 flex-col border-t-2 bg-card outline-none"
    :class="isQuestions || isPlan ? 'border-t-indigo-500/70' : 'border-t-amber-400/70'"
    role="dialog"
    :aria-label="headerTitle"
    @keydown="onKeydown"
  >
    <!-- Questions get a slim single-line header (each question carries its own
         label/text below); permission keeps the fuller title + description. -->
    <div
      class="flex items-center gap-2.5 px-5 border-b border-border shrink-0"
      :class="isQuestions ? 'py-2' : 'py-3 items-start'"
    >
        <component
          :is="isQuestions ? HelpCircle : isPlan ? ClipboardList : hasDiff ? FileEdit : ShieldAlert"
          class="shrink-0"
          :class="[isQuestions ? 'h-4 w-4' : 'h-5 w-5 mt-0.5', isQuestions || isPlan ? 'text-indigo-400' : 'text-amber-400']"
          :stroke-width="1.75"
        />
        <div class="flex-1 min-w-0">
          <h2 class="text-sm font-medium text-foreground" :class="isQuestions ? 'leading-tight' : ''">{{ headerTitle }}</h2>
          <p v-if="isPlan" class="text-xs text-foreground/60 mt-0.5">
            Review the plan below, then approve to start — or keep planning to refine it.
          </p>
          <p v-else-if="!isQuestions && request.description" class="text-xs text-foreground/60 mt-0.5">
            {{ request.description }}
          </p>
          <p v-else-if="!isQuestions" class="text-xs text-foreground/60 mt-0.5">
            Allow this tool call to proceed? You can block it or allow it for this run.
          </p>
        </div>
      </div>

      <!-- AskUserQuestion: one question at a time (stepper). The question header
           is pinned; only its option list scrolls — no stacked sticky headers. -->
      <div v-if="isQuestions && currentQuestion" class="flex-1 min-h-0 flex flex-col">
        <div class="px-5 pt-3 pb-2.5 border-b border-border shrink-0 space-y-1">
          <div class="flex items-center gap-2 flex-wrap">
            <span
              v-if="currentQuestion.header"
              class="text-[10px] uppercase tracking-wider rounded bg-indigo-500/15 text-indigo-600 dark:text-indigo-300 px-1.5 py-0.5"
            >{{ currentQuestion.header }}</span>
            <span class="text-[10px] uppercase tracking-wider text-foreground/40">
              {{ currentQuestion.multiSelect ? 'Select all that apply' : 'Select one' }}
            </span>
          </div>
          <p class="text-sm font-medium text-foreground">{{ currentQuestion.question }}</p>
        </div>

        <div class="flex-1 min-h-0 overflow-auto px-5 py-3 space-y-1.5">
          <button
            v-for="(opt, oi) in currentQuestion.options"
            :key="oi"
            type="button"
            :data-flat="oi"
            class="w-full text-left rounded-md border px-3 py-2 transition-colors"
            :class="[
              isSelected(opt.label)
                ? 'border-indigo-500 bg-indigo-500/10'
                : 'border-border bg-foreground/5 hover:bg-foreground/10',
              isFocused(oi) ? 'ring-2 ring-indigo-400/60 ring-offset-1 ring-offset-card' : '',
            ]"
            @click="toggleOption(opt.label)"
          >
            <div class="flex items-start gap-2.5">
              <span
                class="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center border"
                :class="[
                  currentQuestion.multiSelect ? 'rounded' : 'rounded-full',
                  isSelected(opt.label)
                    ? 'border-indigo-500 bg-indigo-500 text-white'
                    : 'border-foreground/30',
                ]"
              >
                <Check v-if="isSelected(opt.label)" class="h-3 w-3" :stroke-width="3" />
              </span>
              <span class="min-w-0 flex-1">
                <span class="block text-sm text-foreground">{{ opt.label }}</span>
                <span v-if="opt.description" class="block text-xs text-foreground/60 mt-0.5">
                  {{ opt.description }}
                </span>
                <pre
                  v-if="opt.preview"
                  class="mt-1.5 rounded bg-foreground/5 border border-border px-2 py-1.5 text-[11px] font-mono text-foreground/70 whitespace-pre-wrap break-words max-h-48 overflow-auto"
                >{{ opt.preview }}</pre>
              </span>
            </div>
          </button>
          <input
            v-model="other[step]"
            type="text"
            :data-flat="otherIndex"
            placeholder="Other (type your own answer)…"
            class="w-full rounded-md border border-border bg-foreground/5 px-3 py-2 text-sm text-foreground placeholder:text-foreground/40 focus:border-indigo-500 focus:outline-none"
            :class="isOtherFocused() ? 'ring-2 ring-indigo-400/60 ring-offset-1 ring-offset-card' : ''"
            @focus="focusedIndex = otherIndex"
          />
        </div>
      </div>

      <!-- ExitPlanMode: show the proposed plan (rendered markdown), not a raw
           tool-input dump. -->
      <div v-else-if="isPlan" class="flex-1 min-h-0 px-5 py-4 overflow-auto">
        <div
          v-if="planHtml"
          v-html="planHtml"
          class="markdown-output text-sm leading-relaxed text-foreground"
        />
        <pre
          v-else
          class="text-xs font-mono text-foreground/80 whitespace-pre-wrap break-words"
        >{{ planText || 'Claude has no plan details to show.' }}</pre>
      </div>

      <div v-else class="flex-1 min-h-0 px-5 py-4 space-y-3 overflow-auto">
        <div class="flex items-center gap-2 flex-wrap">
          <span class="text-[10px] uppercase tracking-wider text-foreground/40">Tool</span>
          <span class="font-mono text-sm text-indigo-600 dark:text-indigo-300">{{ request.display_name || request.tool_name }}</span>
          <span v-if="filePath" class="font-mono text-xs text-foreground/50 truncate">{{ filePath }}</span>
        </div>

        <!-- Diff view for Edit / MultiEdit / Write -->
        <div v-if="hasDiff" class="space-y-3">
          <div
            v-for="(part, i) in diffParts"
            :key="i"
            class="rounded-md border border-border overflow-hidden font-mono text-[11px] leading-relaxed"
          >
            <div v-if="part.before" class="bg-red-500/10 dark:bg-red-500/5">
              <div
                v-for="(ln, li) in splitLines(part.before)"
                :key="'b' + li"
                class="flex"
              >
                <span class="select-none w-5 text-center text-red-600 dark:text-red-400/70 bg-red-500/15 dark:bg-red-500/10 shrink-0">-</span>
                <span class="px-2 text-red-700 dark:text-red-200/90 whitespace-pre-wrap break-words">{{ ln || ' ' }}</span>
              </div>
            </div>
            <div v-if="part.after" class="bg-emerald-500/10 dark:bg-emerald-500/5">
              <div
                v-for="(ln, li) in splitLines(part.after)"
                :key="'a' + li"
                class="flex"
              >
                <span class="select-none w-5 text-center text-emerald-600 dark:text-emerald-400/70 bg-emerald-500/15 dark:bg-emerald-500/10 shrink-0">+</span>
                <span class="px-2 text-emerald-700 dark:text-emerald-100/90 whitespace-pre-wrap break-words">{{ ln || ' ' }}</span>
              </div>
            </div>
          </div>
        </div>

        <!-- Command / target for bash, web, etc. -->
        <div
          v-else-if="commandPreview"
          class="rounded-md bg-foreground/5 border border-border px-3 py-2"
        >
          <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">Command / target</div>
          <pre class="text-xs font-mono text-foreground whitespace-pre-wrap break-words">{{ commandPreview }}</pre>
        </div>

        <details class="rounded-md bg-foreground/5 border border-border">
          <summary class="px-3 py-2 text-xs text-foreground/60 cursor-pointer select-none">Full input</summary>
          <pre class="px-3 pb-2 text-[11px] font-mono text-foreground/70 whitespace-pre-wrap break-words max-h-80 overflow-auto">{{ inputPreview }}</pre>
        </details>
      </div>

      <div class="@container/footer px-5 py-3 border-t border-border shrink-0">
        <div class="flex flex-col gap-2 @[26rem]/footer:flex-row @[26rem]/footer:items-center">
          <span class="hidden min-w-0 truncate text-[10px] text-foreground/40 @[26rem]/footer:mr-auto @[26rem]/footer:block">
            <template v-if="isQuestions">↑↓ Select · ←→ Nav · ↵ Next · Esc Cancel</template>
            <template v-else-if="isPlan">↵ Approve · Esc Keep planning</template>
            <template v-else>↵ Allow · ⇧↵ Always · Esc Deny</template>
          </span>
          <!-- Action buttons: never wrap; stack full-width only in a narrow panel. -->
          <div class="flex flex-col gap-2 @[26rem]/footer:flex-row @[26rem]/footer:items-center @[26rem]/footer:shrink-0">
            <template v-if="isQuestions">
              <span
                v-if="stepCount > 1"
                class="self-center text-[11px] font-medium text-foreground/50 tabular-nums"
              >{{ step + 1 }}/{{ stepCount }}</span>
              <Button
                v-if="stepCount > 1"
                variant="outline"
                :disabled="step === 0"
                class="w-full @[26rem]/footer:w-auto"
                @click="goPrev"
              >
                <ChevronLeft class="h-3.5 w-3.5" :stroke-width="2" /> Back
              </Button>
              <Button variant="destructive" class="w-full @[26rem]/footer:w-auto" @click="emit('decide', 'deny', false)">
                <X class="h-3.5 w-3.5" :stroke-width="2" /> Cancel
              </Button>
              <Button
                v-if="!isLastStep"
                variant="primary"
                :disabled="!currentAnswered"
                class="w-full @[26rem]/footer:w-auto"
                @click="goNext"
              >
                Next <ChevronRight class="h-3.5 w-3.5" :stroke-width="2" />
              </Button>
              <Button v-else variant="primary" :disabled="!canSubmit" class="w-full @[26rem]/footer:w-auto" @click="submitAnswers">
                <Check class="h-3.5 w-3.5" :stroke-width="2" /> Submit
              </Button>
            </template>
            <template v-else-if="isPlan">
              <Button
                variant="outline"
                class="w-full @[26rem]/footer:w-auto"
                @click="emit('decide', 'deny', false)"
              >
                <X class="h-3.5 w-3.5" :stroke-width="2" /> Keep planning
              </Button>
              <Button
                variant="primary"
                class="w-full @[26rem]/footer:w-auto"
                @click="emit('decide', 'allow', false)"
              >
                <Check class="h-3.5 w-3.5" :stroke-width="2" /> Approve
              </Button>
            </template>
            <template v-else>
              <Button
                variant="destructive"
                class="w-full @[26rem]/footer:w-auto"
                @click="emit('decide', 'deny', false)"
              >
                <X class="h-3.5 w-3.5" :stroke-width="2" /> Deny
              </Button>
              <Button
                v-if="canRemember"
                variant="primary"
                class="w-full @[26rem]/footer:w-auto"
                @click="emit('decide', 'allow', true)"
                title="Allow now and auto-allow this tool for the rest of this run"
              >
                <Check class="h-3.5 w-3.5" :stroke-width="2" /> Always
              </Button>
              <Button
                variant="primary"
                class="w-full @[26rem]/footer:w-auto"
                @click="emit('decide', 'allow', false)"
              >
                <Check class="h-3.5 w-3.5" :stroke-width="2" /> Allow
              </Button>
            </template>
          </div>
        </div>
      </div>
    </div>
</template>

<style scoped>
/* Slide up + brief glow when the panel appears (incl. each new request, since
   RunView keys it by request_id) so it catches the eye without a modal. */
.prompt-attention {
  animation: prompt-in 0.28s ease-out;
}
@keyframes prompt-in {
  0% {
    transform: translateX(16px);
    opacity: 0;
  }
  40% {
    box-shadow: inset 2px 0 0 0 rgba(99, 102, 241, 0.45);
  }
  100% {
    transform: translateX(0);
    opacity: 1;
  }
}
</style>
