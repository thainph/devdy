<script setup lang="ts">
/**
 * The Duo timeline: both sessions' output woven into ONE scrolling conversation.
 *
 * Replaces the old two-column layout — with two independent scrollers you had to
 * ping-pong between columns to follow a single exchange, and each column was too
 * narrow for code/diffs. Per-turn stream detail is attached by
 * {@link buildDuoConversation}; see that module for the mapping rules.
 *
 * Permission / question prompts keep the SAME look and position as the session
 * screen (a resizable drawer sliding in from the right — see RunView) — the only
 * duo-specific part is that both sessions' queues are merged so a question can
 * never be missed, whichever side raised it.
 */
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ClipboardCopy, Check, MessagesSquare, CheckCircle2, AlertTriangle, Square } from 'lucide-vue-next'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useOrchestratorStore, type DuoRole } from '@/stores/orchestrator'
import { buildDuoConversation, conversationToMarkdown, type DuoConversationInput } from '@/lib/duoConversation'
import { useToast } from '@/composables/useToast'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import DuoMessage from './DuoMessage.vue'

defineProps<{ renderText: (md: string) => string }>()
const emit = defineEmits<{ openSession: [runId: string] }>()

const { t } = useI18n()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const orch = useOrchestratorStore()
const { toast } = useToast()

// ── Timeline ────────────────────────────────────────────────────────────────
const conversationInput = computed<DuoConversationInput>(() => {
  const s = orch.state
  return {
    turns: s.turns,
    goal: s.goal,
    round: s.round,
    phase: s.phase,
    stopReason: s.stopReason,
    waiting: s.waiting,
    active: s.active,
    designerLabel: s.designerLabel,
    reviewerLabel: s.reviewerLabel,
    designerEngine: s.designerEngine,
    reviewerEngine: s.reviewerEngine,
    designerRunId: s.designerRunId,
    reviewerRunId: s.reviewerRunId,
  }
})

const messages = computed(() =>
  buildDuoConversation(
    conversationInput.value,
    (runId) => live.get(runId)?.entries ?? [],
    (runId) => live.get(runId)?.status === 'running',
  ),
)

const filter = ref<'all' | DuoRole>('all')
const visibleMessages = computed(() =>
  filter.value === 'all'
    ? messages.value
    : messages.value.filter((m) => (m.kind === 'turn' || m.kind === 'live' ? m.role === filter.value : m.kind !== 'round')),
)

const hasContent = computed(() => messages.value.some((m) => m.kind === 'turn' || m.kind === 'live'))

const stopReasonLabel = computed(() => {
  switch (orch.state.stopReason) {
    case 'consensus': return t('duo.reason.consensus')
    case 'maxRounds': return t('duo.reason.maxRounds')
    case 'manual': return t('duo.reason.manual')
    case 'error': return orch.state.error || t('duo.reason.error')
    default: return ''
  }
})

const copiedAll = ref(false)
async function copyTranscript() {
  try {
    await navigator.clipboard.writeText(conversationToMarkdown(conversationInput.value))
    copiedAll.value = true
    setTimeout(() => { copiedAll.value = false }, 1500)
  } catch { /* clipboard unavailable */ }
}

// ── Permission / question queue, merged across BOTH sessions ────────────────
// The side that isn't running can still hold a stale request (e.g. its turn was
// cancelled mid-tool), so we never filter by `state.waiting` — that would strand
// the duo on a question the user can't see. The running side just goes first.
const duoPermissions = computed(() => {
  const sides: Array<{ role: DuoRole; runId: string | null }> = [
    { role: 'designer', runId: orch.state.designerRunId },
    { role: 'reviewer', runId: orch.state.reviewerRunId },
  ]
  if (orch.state.waiting === 'reviewer') sides.reverse()
  return sides.flatMap(({ role, runId }) => {
    const request = runId ? live.get(runId)?.permissionQueue[0] : undefined
    return request ? [{ role, runId: runId as string, request }] : []
  })
})
const headPermission = computed(() => duoPermissions.value[0] ?? null)

/** Tool name of a side's pending request — drives the marker on its message. */
function pendingToolFor(role: DuoRole): string | null {
  return duoPermissions.value.find((p) => p.role === role)?.request.tool_name ?? null
}

const headLabel = computed(() => {
  const head = headPermission.value
  if (!head) return ''
  return head.role === 'designer' ? orch.state.designerLabel : orch.state.reviewerLabel
})
const headEngine = computed(() => {
  const head = headPermission.value
  if (!head) return ''
  return head.role === 'designer' ? orch.state.designerEngine : orch.state.reviewerEngine
})

function allowedToolsFor(runId: string): string[] {
  return live.get(runId)?.allowedTools ?? []
}

async function handleDecide(decision: 'allow' | 'deny' | 'ask', remember: boolean) {
  const head = headPermission.value
  if (!head) return
  live.shiftPermission(head.runId)
  if (remember && decision === 'allow') live.rememberAllowedTool(head.runId, head.request.tool_name)
  else if (remember && decision === 'deny') live.rememberDeniedTool(head.runId, head.request.tool_name)
  try {
    await runsStore.respondPermission(head.request.run_id, head.request.request_id, decision)
  } catch (e) {
    toast.error(t('run.permissionResponseFailed', { error: String(e) }))
  }
}

async function handleAnswer(answers: Record<string, string>) {
  const head = headPermission.value
  if (!head) return
  live.shiftPermission(head.runId)
  try {
    await runsStore.respondPermission(head.request.run_id, head.request.request_id, 'allow', undefined, { answers })
  } catch (e) {
    toast.error(t('run.permissionResponseFailed', { error: String(e) }))
  }
}

// Drawer width, resizable from its left edge (same behaviour as the session
// screen's question panel).
const wrapEl = ref<HTMLElement | null>(null)
const questionWidthPct = ref(42)
const isResizingQuestion = ref(false)

function startQuestionResize(e: MouseEvent) {
  e.preventDefault()
  isResizingQuestion.value = true
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
  window.addEventListener('mousemove', onQuestionResize)
  window.addEventListener('mouseup', stopQuestionResize)
}
function onQuestionResize(e: MouseEvent) {
  if (!wrapEl.value) return
  const rect = wrapEl.value.getBoundingClientRect()
  questionWidthPct.value = Math.max(20, Math.min(80, ((rect.right - e.clientX) / rect.width) * 100))
}
function stopQuestionResize() {
  isResizingQuestion.value = false
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  window.removeEventListener('mousemove', onQuestionResize)
  window.removeEventListener('mouseup', stopQuestionResize)
}
onBeforeUnmount(stopQuestionResize)

// ── Follow-the-tail scrolling ───────────────────────────────────────────────
// Auto-scroll only while the user is already at the bottom; if they scrolled up
// to re-read something, leave their position alone and offer a jump button.
const scrollEl = ref<HTMLElement | null>(null)
const stickToBottom = ref(true)
const pointerDown = ref(false)
const SCROLL_PIN_THRESHOLD = 80

function isNearBottom(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= SCROLL_PIN_THRESHOLD
}
function onScroll() {
  // Don't re-pin mid drag-select: the pointer may be dragging past the bottom.
  if (scrollEl.value && !pointerDown.value) stickToBottom.value = isNearBottom(scrollEl.value)
}
function scrollToBottom() {
  const el = scrollEl.value
  if (el) el.scrollTop = el.scrollHeight
}
function jumpToLatest() {
  stickToBottom.value = true
  scrollToBottom()
}

// Deep watch: text grows INSIDE the last message as it streams, which doesn't
// change the array identity.
watch(
  messages,
  () => { if (stickToBottom.value) nextTick(scrollToBottom) },
  { deep: true, flush: 'post' },
)
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <!-- Toolbar -->
    <div v-if="hasContent" class="flex items-center gap-2 border-b border-border/60 px-4 py-1.5 shrink-0">
      <div class="inline-flex rounded-md border border-border p-0.5 text-[11px]">
        <button
          v-for="opt in (['all', 'designer', 'reviewer'] as const)"
          :key="opt"
          class="px-2 py-0.5 rounded-[4px] transition-colors cursor-pointer"
          :class="filter === opt ? 'bg-accent text-foreground font-medium' : 'text-muted-foreground hover:text-foreground'"
          @click="filter = opt"
        >
          {{ opt === 'all' ? t('duo.conversation.filterAll')
            : opt === 'designer' ? orch.state.designerLabel : orch.state.reviewerLabel }}
        </button>
      </div>
      <button
        class="ml-auto inline-flex items-center gap-1 h-6 px-1.5 rounded text-[11px] text-foreground/50 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
        :title="t('duo.conversation.copyTranscript')"
        @click="copyTranscript"
      >
        <Check v-if="copiedAll" class="h-3.5 w-3.5 text-emerald-500" :stroke-width="2" />
        <ClipboardCopy v-else class="h-3.5 w-3.5" :stroke-width="2" />
        {{ t('duo.conversation.copyTranscript') }}
      </button>
    </div>

    <div ref="wrapEl" class="relative flex-1 min-h-0">
      <div
        ref="scrollEl"
        class="absolute inset-0 overflow-y-auto px-4 py-3 space-y-3"
        @scroll="onScroll"
        @pointerdown="pointerDown = true"
        @pointerup="pointerDown = false"
      >
        <div v-if="!hasContent" class="flex h-full items-center justify-center">
          <div class="max-w-xs text-center">
            <MessagesSquare class="mx-auto mb-3 h-9 w-9 text-foreground/15" :stroke-width="1" />
            <p class="text-xs text-muted-foreground">{{ t('duo.conversation.empty') }}</p>
          </div>
        </div>

        <template v-for="m in visibleMessages" :key="m.id">
          <!-- Topic: the goal that seeded the exchange. -->
          <div v-if="m.kind === 'topic'" class="rounded-lg border border-dashed border-border bg-muted/30 px-4 py-2.5">
            <p class="text-[10px] font-medium uppercase tracking-wide text-muted-foreground mb-1">
              {{ t('duo.conversation.topic') }}
            </p>
            <p class="text-sm leading-relaxed whitespace-pre-wrap">{{ m.goal }}</p>
          </div>

          <div v-else-if="m.kind === 'round'" class="flex items-center gap-3 py-1">
            <span class="h-px flex-1 bg-border" />
            <span class="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
              {{ t('duo.conversation.round', { n: m.round }) }}
            </span>
            <span class="h-px flex-1 bg-border" />
          </div>

          <div v-else-if="m.kind === 'notice'"
            class="flex items-center gap-2 rounded-md px-3 py-2 text-xs"
            :class="m.phase === 'done' ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
              : m.phase === 'error' ? 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
              : 'bg-muted text-muted-foreground'"
          >
            <component
              :is="m.phase === 'done' ? CheckCircle2 : m.phase === 'error' ? AlertTriangle : Square"
              class="h-3.5 w-3.5 shrink-0" :stroke-width="2"
            />
            {{ stopReasonLabel }}
          </div>

          <DuoMessage
            v-else
            :message="m"
            :render-text="renderText"
            :pending-tool="pendingToolFor(m.role)"
            @open-session="(id) => emit('openSession', id)"
          />
        </template>
      </div>

      <button
        v-if="hasContent && !stickToBottom"
        class="absolute bottom-4 right-4 z-10 flex items-center gap-1 rounded-full border border-border bg-card/90 px-3 py-1.5 text-[11px] font-mono text-foreground/80 shadow-md backdrop-blur transition-colors hover:bg-card hover:text-foreground cursor-pointer"
        @click="jumpToLatest"
      >
        <ChevronDown class="h-3.5 w-3.5" :stroke-width="2" />
        {{ t('duo.conversation.jumpLatest') }}
      </button>

      <!-- Question / permission drawer — same overlay-from-the-right panel as
           the session screen, so it floats above the conversation instead of
           reflowing it (the reading position never jumps). -->
      <div
        v-if="headPermission"
        class="absolute inset-y-0 right-0 z-20 flex bg-card border-l border-border shadow-[-8px_0_24px_-12px_rgba(0,0,0,0.45)]"
        :style="{ width: questionWidthPct + '%' }"
      >
        <div
          class="group relative shrink-0 w-px bg-border cursor-col-resize select-none"
          :class="{ 'bg-primary/60': isResizingQuestion }"
          @mousedown="startQuestionResize"
        >
          <div
            class="absolute inset-y-0 -left-1.5 -right-1.5 z-10 transition-colors group-hover:bg-primary/30"
            :class="{ 'bg-primary/40': isResizingQuestion }"
          />
        </div>
        <div class="relative flex-1 min-w-0 min-h-0 overflow-auto">
          <!-- Unlike the session screen there are two possible askers, so name
               the side that raised this one. -->
          <div class="flex items-center gap-2 border-b border-border px-3 py-1.5">
            <span
              class="flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[10px] font-semibold"
              :class="headPermission.role === 'designer'
                ? 'bg-primary/10 text-primary'
                : 'bg-violet-500/10 text-violet-600 dark:text-violet-400'"
            >{{ (headLabel.trim()[0] || '?').toUpperCase() }}</span>
            <span class="text-xs font-semibold truncate">{{ headLabel }}</span>
            <span class="text-[11px] font-mono text-muted-foreground">{{ headEngine }}</span>
            <span v-if="duoPermissions.length > 1" class="ml-auto text-[10px] text-muted-foreground">
              {{ t('duo.conversation.morePending', { n: duoPermissions.length - 1 }) }}
            </span>
          </div>
          <PermissionPrompt
            :key="headPermission.request.request_id"
            :request="headPermission.request"
            :allowed-tools="allowedToolsFor(headPermission.runId)"
            :render-text="renderText"
            @decide="handleDecide"
            @answer="handleAnswer"
          />
        </div>
      </div>
    </div>
  </div>
</template>
