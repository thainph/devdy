<script setup lang="ts">
/**
 * Controller-side permission prompt (FR-006/FR-007). Deliberately a slim,
 * self-contained component rather than reusing the app's `PermissionPrompt.vue`
 * verbatim, because that one exposes "Allow always" / "Deny always" buttons —
 * which are FORBIDDEN from a Controller (BR-016 / SEC-011 / AC-17). Here the
 * ONLY two decisions are Allow once and Deny once, so the forbidden decision is
 * not even representable in the UI.
 *
 * For `AskUserQuestion` there is no "allow/deny" — Claude is asking the user to
 * pick from structured options. We mirror the desktop `PermissionPrompt.vue`
 * (single/multi select + a freeform "Other"), then submit the selections as
 * `answers` alongside an `allow_once` decision (the once-only gate is unchanged).
 *
 * It reuses the shared design tokens, the `Button` primitive and `DiffView` so
 * it stays visually consistent with the main app.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ShieldAlert, FileEdit, HelpCircle, Check, X } from 'lucide-vue-next'
import { Button } from '@/components/ui'

const { t } = useI18n()
import DiffView from '@/components/DiffView.vue'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import type { QuestionAnswers, RemoteDecision } from '../protocol'

const props = defineProps<{ request: PermissionRequest }>()
const emit = defineEmits<{
  decide: [decision: RemoteDecision]
  answer: [answers: QuestionAnswers]
}>()

function asObject(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}
function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

const input = computed(() => asObject(props.request.tool_input))

const filePath = computed(() => {
  const p = input.value['file_path'] ?? input.value['path']
  return typeof p === 'string' ? p : ''
})

// --- AskUserQuestion --------------------------------------------------------
const isQuestions = computed(() => props.request.tool_name === 'AskUserQuestion')

interface QuestionOption {
  label: string
  description: string
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
            return { label: str(oo['label']), description: str(oo['description']) }
          })
          .filter((x) => x.label)
      : []
    return {
      question: str(o['question']),
      header: str(o['header']),
      multiSelect: o['multiSelect'] === true,
      options: opts,
    }
  })
})

// Per-question selected labels + freeform "Other" text, indexed by question.
const selected = ref<string[][]>([])
const other = ref<string[]>([])

watch(
  questions,
  (qs) => {
    selected.value = qs.map(() => [])
    other.value = qs.map(() => '')
  },
  { immediate: true },
)

function isSelected(qi: number, label: string): boolean {
  return (selected.value[qi] ?? []).includes(label)
}

function toggleOption(qi: number, label: string): void {
  const q = questions.value[qi]
  if (!q) return
  const cur = selected.value[qi] ?? []
  if (q.multiSelect) {
    selected.value[qi] = cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label]
  } else {
    selected.value[qi] = cur.includes(label) ? [] : [label]
  }
}

/** Combined answer for a question: selected labels plus any "Other" text. */
function answerFor(qi: number): string {
  const parts = [...(selected.value[qi] ?? [])]
  const free = (other.value[qi] ?? '').trim()
  if (free) parts.push(free)
  return parts.join(', ')
}

const canSubmit = computed(
  () => questions.value.length > 0 && questions.value.every((_, i) => answerFor(i).length > 0),
)

function submitAnswers(): void {
  if (!canSubmit.value) return
  const answers: QuestionAnswers = {}
  questions.value.forEach((q, i) => {
    answers[q.question] = answerFor(i)
  })
  emit('answer', answers)
}

// --- Generic tool permission (Edit/Write/Bash/…) ---------------------------
interface DiffPart {
  before: string
  after: string
}
const diffParts = computed<DiffPart[]>(() => {
  const name = props.request.tool_name
  const inp = input.value
  if (name === 'Edit') {
    const before = str(inp['old_string'])
    const after = str(inp['new_string'])
    if (before || after) return [{ before, after }]
  }
  if (name === 'MultiEdit' && Array.isArray(inp['edits'])) {
    return (inp['edits'] as unknown[])
      .map((e) => asObject(e))
      .map((e) => ({ before: str(e['old_string']), after: str(e['new_string']) }))
      .filter((d) => d.before || d.after)
  }
  if (name === 'Write') {
    const after = str(inp['content'])
    if (after) return [{ before: '', after }]
  }
  return []
})
const hasDiff = computed(() => diffParts.value.length > 0)

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
</script>

<template>
  <div
    class="flex h-full min-h-0 flex-col border-t-2 bg-card"
    :class="isQuestions ? 'border-t-indigo-500/70' : 'border-t-amber-400/70'"
    role="dialog"
    :aria-label="isQuestions ? t('permission.controller.questionAria') : t('permission.controller.requestAria')"
  >
    <div class="flex items-start gap-2.5 px-4 py-3 border-b border-border shrink-0">
      <component
        :is="isQuestions ? HelpCircle : hasDiff ? FileEdit : ShieldAlert"
        class="h-5 w-5 mt-0.5 shrink-0"
        :class="isQuestions ? 'text-indigo-400' : 'text-amber-400'"
        :stroke-width="1.75"
      />
      <div class="flex-1 min-w-0">
        <h2 class="text-sm font-medium text-foreground">
          {{ isQuestions ? t('permission.controller.questionTitle') : t('permission.controller.requestTitle') }}
        </h2>
        <p class="text-xs text-foreground/60 mt-0.5">
          <template v-if="isQuestions">{{ t('permission.controller.questionHint') }}</template>
          <template v-else>
            {{ t('permission.controller.requestHint') }}
          </template>
        </p>
      </div>
    </div>

    <!-- AskUserQuestion: structured options (single/multi select + Other). -->
    <div v-if="isQuestions" class="flex-1 min-h-0 px-4 py-4 space-y-4 overflow-auto">
      <div v-for="(q, qi) in questions" :key="qi" class="space-y-2">
        <div class="flex items-center gap-2 flex-wrap">
          <span
            v-if="q.header"
            class="text-[10px] uppercase tracking-wider rounded bg-indigo-500/15 text-indigo-600 dark:text-indigo-300 px-1.5 py-0.5"
            >{{ q.header }}</span
          >
          <span class="text-[10px] uppercase tracking-wider text-foreground/40">
            {{ q.multiSelect ? t('permission.selectAll') : t('permission.selectOne') }}
          </span>
        </div>
        <p class="text-sm font-medium text-foreground">{{ q.question }}</p>
        <div class="space-y-1.5">
          <button
            v-for="(opt, oi) in q.options"
            :key="oi"
            type="button"
            class="w-full text-left rounded-md border px-3 py-2 transition-colors"
            :class="
              isSelected(qi, opt.label)
                ? 'border-indigo-500 bg-indigo-500/10'
                : 'border-border bg-foreground/5 hover:bg-foreground/10'
            "
            @click="toggleOption(qi, opt.label)"
          >
            <div class="flex items-start gap-2.5">
              <span
                class="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center border"
                :class="[
                  q.multiSelect ? 'rounded' : 'rounded-full',
                  isSelected(qi, opt.label)
                    ? 'border-indigo-500 bg-indigo-500 text-white'
                    : 'border-foreground/30',
                ]"
              >
                <Check v-if="isSelected(qi, opt.label)" class="h-3 w-3" :stroke-width="3" />
              </span>
              <span class="min-w-0 flex-1">
                <span class="block text-sm text-foreground">{{ opt.label }}</span>
                <span v-if="opt.description" class="block text-xs text-foreground/60 mt-0.5">
                  {{ opt.description }}
                </span>
              </span>
            </div>
          </button>
          <input
            v-model="other[qi]"
            type="text"
            :placeholder="t('permission.otherPlaceholder')"
            class="w-full rounded-md border border-border bg-foreground/5 px-3 py-2 text-sm text-foreground placeholder:text-foreground/40 focus:border-indigo-500 focus:outline-none"
          />
        </div>
      </div>
    </div>

    <!-- Generic tool permission. -->
    <div v-else class="flex-1 min-h-0 px-4 py-4 space-y-3 overflow-auto">
      <div class="flex items-center gap-2 flex-wrap">
        <span class="text-[10px] uppercase tracking-wider text-foreground/40">{{ t('permission.tool') }}</span>
        <span class="font-mono text-sm text-indigo-600 dark:text-indigo-300">{{ request.tool_name }}</span>
        <span v-if="filePath" class="font-mono text-xs text-foreground/50 truncate">{{ filePath }}</span>
      </div>
      <span v-if="request.cwd" class="block font-mono text-[11px] text-foreground/40 truncate">
        cwd: {{ request.cwd }}
      </span>

      <div v-if="hasDiff" class="space-y-3">
        <DiffView v-for="(part, i) in diffParts" :key="i" :before="part.before" :after="part.after" />
      </div>

      <div
        v-else-if="commandPreview"
        class="rounded-md bg-foreground/5 border border-border px-3 py-2"
      >
        <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">{{ t('permission.commandTarget') }}</div>
        <pre class="text-xs font-mono text-foreground whitespace-pre-wrap break-words">{{ commandPreview }}</pre>
      </div>

      <details class="rounded-md bg-foreground/5 border border-border">
        <summary class="px-3 py-2 text-xs text-foreground/60 cursor-pointer select-none">{{ t('permission.fullInput') }}</summary>
        <pre class="px-3 pb-2 text-[11px] font-mono text-foreground/70 whitespace-pre-wrap break-words max-h-80 overflow-auto">{{ inputPreview }}</pre>
      </details>
    </div>

    <!-- AskUserQuestion submits `answers` (with allow_once); other tools get the
         ONLY two Controller decisions: Allow once / Deny once (AC-17 / BR-016). -->
    <div class="px-4 py-3 border-t border-border shrink-0 flex flex-col gap-2 sm:flex-row sm:justify-end">
      <template v-if="isQuestions">
        <Button variant="destructive" class="w-full sm:w-auto" @click="emit('decide', 'deny_once')">
          <X class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('permission.controller.cancel') }}
        </Button>
        <Button variant="primary" :disabled="!canSubmit" class="w-full sm:w-auto" @click="submitAnswers">
          <Check class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('permission.submit') }}
        </Button>
      </template>
      <template v-else>
        <Button variant="destructive" class="w-full sm:w-auto" @click="emit('decide', 'deny_once')">
          <X class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('permission.controller.denyOnce') }}
        </Button>
        <Button variant="primary" class="w-full sm:w-auto" @click="emit('decide', 'allow_once')">
          <Check class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('permission.controller.allowOnce') }}
        </Button>
      </template>
    </div>
  </div>
</template>
