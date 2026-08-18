<script setup lang="ts">
/**
 * Single-run focus screen (Phase 4): the connection/trust header, the scrollable
 * {@link StreamLog} (auto-scroll-to-bottom), the {@link ControllerPermissionPrompt}
 * when a permission is pending, and the {@link SessionComposer} at the bottom.
 *
 * Backend-agnostic: state comes in as props; user intent goes out as events. The
 * parent (`App.vue`) wires events to the {@link ControllerConnection}.
 */
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertTriangle, Loader2, ShieldCheck, Wifi, WifiOff, X,
} from 'lucide-vue-next'
import StreamLog from '@/components/StreamLog.vue'
import type { StreamEntry } from '@/lib/streamEvents'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import ControllerPermissionPrompt from './ControllerPermissionPrompt.vue'
import SessionComposer, { type ComposerTurn } from './SessionComposer.vue'
import SessionUsageBar from './SessionUsageBar.vue'
import SessionPlanUsageBar from './SessionPlanUsageBar.vue'
import type { UsageInfo } from '../runStore'
import type { RateLimitWindows } from '@/stores/liveRuns'
import type { ConnStatus } from '../connection'
import type { PlanBudget, QuestionAnswers, RemoteDecision, SlashCommand } from '../protocol'

const props = defineProps<{
  title: string
  status: ConnStatus
  live: boolean
  hostFingerprint: string | null
  decodeErrors: number
  lastError: string | null
  notice: string | null
  entries: StreamEntry[]
  running: boolean
  pending: PermissionRequest | null
  renderText: (md: string) => string
  slashCommands: SlashCommand[]
  projectFiles: string[]
  engine: string
  model: string
  permissionMode: string
  sending: boolean
  /** Context-window occupancy (tokens) for the usage bar. */
  contextTokens: number
  /** Model id from system.init (resolves the context limit). */
  contextModel: string | null
  /** claude.ai rate-limit windows, if reported. */
  rateLimit: RateLimitWindows | null
  /** Latest turn's usage (tokens + cost). */
  usage: UsageInfo | null
  /** Subscription plan-usage badges (mirror of the desktop BudgetBadge). */
  claudeBudget: PlanBudget | null
  codexBudget: PlanBudget | null
}>()

const emit = defineEmits<{
  send: [turn: ComposerTurn]
  cancel: []
  decide: [decision: RemoteDecision]
  answer: [answers: QuestionAnswers]
  disconnect: []
  clearNotice: []
  'update:engine': [value: string]
  'update:model': [value: string]
  'update:permissionMode': [value: string]
  requestFiles: []
}>()

const { t } = useI18n()

const isActive = computed(() => props.status === 'connected')

const statusLabel = computed(() => {
  switch (props.status) {
    case 'joining':
      return t('controller.session.joining')
    case 'handshaking':
      return t('controller.session.securing')
    case 'connected':
      return props.live ? t('controller.session.connected') : t('controller.session.connectedWaiting')
    case 'reconnecting':
      return t('controller.session.reconnecting')
    case 'error':
      return t('controller.session.error')
    case 'disconnected':
      return t('controller.session.disconnected')
    default:
      return t('controller.session.idle')
  }
})

const shortFp = computed(() => (props.hostFingerprint ? props.hostFingerprint.slice(0, 12) : ''))

// ── auto-scroll ─────────────────────────────────────────────────────────────
const mainEl = ref<HTMLElement | null>(null)
const stickToBottom = ref(true)
const pointerDownInStream = ref(false)
let lastStreamInteractionAt = 0
const STREAM_INTERACTION_COOLDOWN = 600

function scrollToBottom(): void {
  const el = mainEl.value
  if (el) el.scrollTop = el.scrollHeight
}

function onScroll(): void {
  const el = mainEl.value
  if (!el) return
  stickToBottom.value = el.scrollHeight - el.scrollTop - el.clientHeight < 80
}

function onStreamPointerDown(): void {
  pointerDownInStream.value = true
  lastStreamInteractionAt = Date.now()
}

function onWindowPointerUp(): void {
  if (pointerDownInStream.value) lastStreamInteractionAt = Date.now()
  pointerDownInStream.value = false
}

function hasStreamSelection(): boolean {
  const selection = window.getSelection()
  const el = mainEl.value
  if (!selection || selection.isCollapsed || !el) return false
  return (
    (selection.anchorNode !== null && el.contains(selection.anchorNode)) ||
    (selection.focusNode !== null && el.contains(selection.focusNode))
  )
}

function shouldAutoScroll(): boolean {
  return (
    stickToBottom.value &&
    !pointerDownInStream.value &&
    Date.now() - lastStreamInteractionAt >= STREAM_INTERACTION_COOLDOWN &&
    !hasStreamSelection()
  )
}

watch(
  () => props.entries.length,
  () => {
    if (!shouldAutoScroll()) return
    // Re-check after Vue patches the stream: the user may start selecting text
    // after this watcher queued the update but before the DOM flush completes.
    void nextTick(() => {
      if (shouldAutoScroll()) scrollToBottom()
    })
  },
)

onMounted(() => window.addEventListener('pointerup', onWindowPointerUp))
onUnmounted(() => window.removeEventListener('pointerup', onWindowPointerUp))
</script>

<template>
  <div class="flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <!-- Top bar -->
    <header class="shrink-0 flex items-center gap-2 px-3 py-2 border-b border-border">
      <span
        class="inline-flex items-center gap-1.5 text-xs font-medium min-w-0"
        :class="isActive && live ? 'text-emerald-500' : status === 'error' ? 'text-destructive' : 'text-amber-500'"
      >
        <Loader2
          v-if="status === 'joining' || status === 'handshaking' || status === 'reconnecting' || (isActive && !live)"
          class="h-3.5 w-3.5 animate-spin"
          :stroke-width="2"
        />
        <Wifi v-else-if="isActive" class="h-3.5 w-3.5" :stroke-width="2" />
        <WifiOff v-else class="h-3.5 w-3.5" :stroke-width="2" />
        <span class="truncate">{{ statusLabel }}</span>
      </span>
      <span
        v-if="shortFp"
        class="inline-flex items-center gap-1 text-[10px] font-mono text-foreground/45"
        :title="t('controller.session.verifiedFingerprint', { fp: hostFingerprint })"
      >
        <ShieldCheck class="h-3 w-3 text-emerald-500" :stroke-width="2" /> {{ shortFp }}…
      </span>
      <div class="ml-auto flex items-center gap-1.5">
        <span
          v-if="decodeErrors > 0"
          class="inline-flex items-center gap-1 text-[10px] text-amber-500"
          :title="t('controller.session.decodeErrorsTitle')"
        >
          <AlertTriangle class="h-3 w-3" :stroke-width="2" /> {{ decodeErrors }}
        </span>
        <button
          class="inline-flex items-center justify-center h-7 w-7 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
          :title="t('controller.session.disconnect')"
          @click="emit('disconnect')"
        >
          <X class="h-4 w-4" :stroke-width="2" />
        </button>
      </div>
    </header>

    <!-- Subscription usage (Claude + Codex), mirror of the desktop BudgetBadge.
         Kept above the run title so plan utilization is the first thing seen. -->
    <SessionPlanUsageBar :claude="claudeBudget" :codex="codexBudget" />

    <!-- Run title -->
    <div class="shrink-0 px-3 py-1.5 border-b border-border/60">
      <p class="text-xs font-medium truncate">{{ title }}</p>
    </div>

    <!-- Reconnect / error banner -->
    <div
      v-if="lastError && status !== 'connected'"
      class="shrink-0 px-3 py-1.5 text-xs text-center"
      :class="status === 'error' ? 'bg-destructive/15 text-destructive' : 'bg-amber-500/15 text-amber-600 dark:text-amber-400'"
    >
      {{ lastError }}
    </div>

    <!-- Host advisory -->
    <div
      v-if="notice"
      class="shrink-0 px-3 py-1.5 text-xs text-center bg-muted/60 text-foreground/70 flex items-center justify-center gap-2"
    >
      {{ notice }}
      <button class="text-foreground/40 hover:text-foreground" @click="emit('clearNotice')">
        <X class="h-3 w-3" :stroke-width="2" />
      </button>
    </div>

    <!-- Stream -->
    <main
      ref="mainEl"
      class="flex-1 min-h-0 overflow-auto px-3 py-3"
      @scroll="onScroll"
      @pointerdown="onStreamPointerDown"
    >
      <StreamLog :entries="entries" :running="running" :render-text="renderText" />
    </main>

    <!-- Usage / context status bar (mirrors the desktop ContextMeter). -->
    <SessionUsageBar
      :tokens="contextTokens"
      :model="contextModel"
      :engine="engine"
      :rate-limit="rateLimit"
      :usage="usage"
    />

    <!-- Permission prompt takes over the bottom region when pending. -->
    <div v-if="pending" class="shrink-0 max-h-[60dvh]">
      <ControllerPermissionPrompt
        :request="pending"
        @decide="emit('decide', $event)"
        @answer="emit('answer', $event)"
      />
    </div>

    <!-- Composer -->
    <SessionComposer
      v-else
      :slash-commands="slashCommands"
      :project-files="projectFiles"
      :engine="engine"
      :model="model"
      :permission-mode="permissionMode"
      :running="running"
      :sending="sending"
      :disabled="!isActive"
      @send="emit('send', $event)"
      @cancel="emit('cancel')"
      @update:engine="emit('update:engine', $event)"
      @update:model="emit('update:model', $event)"
      @update:permission-mode="emit('update:permissionMode', $event)"
      @request-files="emit('requestFiles')"
    />
  </div>
</template>
