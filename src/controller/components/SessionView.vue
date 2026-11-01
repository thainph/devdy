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
  AlertTriangle, ArrowDown, ChevronLeft, Loader2, ShieldCheck, Wifi, WifiOff, X,
} from 'lucide-vue-next'
import StreamLog from '@/components/StreamLog.vue'
import type { StreamEntry } from '@/lib/streamEvents'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import ControllerPermissionPrompt from './ControllerPermissionPrompt.vue'
import HeaderButton from './HeaderButton.vue'
import SessionComposer, { type ComposerTurn } from './SessionComposer.vue'
import SessionUsageBar from './SessionUsageBar.vue'
import SessionPlanUsageBar from './SessionPlanUsageBar.vue'
import type { UsageInfo } from '../runStore'
import type { RateLimitWindows } from '@/stores/liveRuns'
import type { ConnStatus } from '../connection'
import type {
  ClaudeAccountChoice, ClaudeAccountUsage, FetchedModel, PlanBudget, QuestionAnswers,
  RemoteDecision, SlashCommand,
} from '../protocol'

const props = defineProps<{
  /** The run (or draft) on screen; a change re-pins the view to the bottom. */
  runKey: string | null
  /** The Host accepts Always from a Controller. */
  allowAlways?: boolean
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
  /** Models the Host discovered from the account (composer model selector). */
  discoveredModels: { codex: FetchedModel[] }
  /** Managed Claude accounts a run may be switched to. */
  claudeAccountChoices: ClaudeAccountChoice[]
  /** The account the run executes with; `''` = global `~/.claude`. */
  claudeAccountId: string
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
  /** Label of the Claude account the bound run is using (null = global/legacy). */
  claudeAccount: string | null
  /** Every managed Claude account with its own usage (desktop badge parity). */
  claudeAccounts: ClaudeAccountUsage[]
  /** Other runs awaiting a permission decision. Surfaced as a tappable banner
   * (to the Inbox) so a prompt in a run the user is not watching is never
   * silently queued. */
  pendingElsewhere: string[]
}>()

const emit = defineEmits<{
  send: [turn: ComposerTurn]
  cancel: []
  decide: [decision: RemoteDecision]
  answer: [answers: QuestionAnswers]
  disconnect: []
  back: []
  openInbox: []
  clearNotice: []
  'update:engine': [value: string]
  'update:model': [value: string]
  'update:permissionMode': [value: string]
  'update:claudeAccountId': [value: string]
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

/** Land on the newest message: on open, on switching run and after a history
 * replay (which swaps the whole entries array, often from empty). */
function pinToBottom(): void {
  stickToBottom.value = true
  void nextTick(() => {
    scrollToBottom()
    // Markdown / images may lay out a frame later; the ResizeObserver keeps up
    // after that, this covers the first paint.
    requestAnimationFrame(scrollToBottom)
  })
}

watch(() => props.runKey, pinToBottom)
watch(
  () => props.entries,
  (next, prev) => {
    if (!prev?.length && next.length) pinToBottom()
  },
)

/** Jump back down from the floating "Newest" button. */
function jumpToNewest(): void {
  stickToBottom.value = true
  scrollToBottom()
}

// Content grows without a scroll event (Markdown rendering, images, a tool
// result expanding) and the viewport shrinks when the permission prompt opens.
// Either way: if the user was at the bottom, keep them there.
const contentEl = ref<HTMLElement | null>(null)
let resizeObserver: ResizeObserver | null = null

onMounted(() => {
  window.addEventListener('pointerup', onWindowPointerUp)
  if (typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(() => {
      if (shouldAutoScroll()) scrollToBottom()
    })
    if (mainEl.value) resizeObserver.observe(mainEl.value)
    if (contentEl.value) resizeObserver.observe(contentEl.value)
  }
  pinToBottom()
})
onUnmounted(() => {
  window.removeEventListener('pointerup', onWindowPointerUp)
  resizeObserver?.disconnect()
})
</script>

<template>
  <div class="flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <!-- Top bar -->
    <header class="shrink-0 flex items-center gap-2 px-3 py-2 border-b border-border">
      <HeaderButton class="-ml-1" :label="t('controller.browser.backToRuns')" @click="emit('back')">
        <ChevronLeft class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
      <span
        class="inline-flex items-center gap-1.5 text-xs font-medium min-w-0"
        :class="isActive && live ? 'text-success' : status === 'error' ? 'text-destructive' : 'text-warning'"
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
        <ShieldCheck class="h-3 w-3 text-success" :stroke-width="2" /> {{ shortFp }}…
      </span>
      <div class="ml-auto flex items-center gap-1.5">
        <span
          v-if="decodeErrors > 0"
          class="inline-flex items-center gap-1 text-[10px] text-warning"
          :title="t('controller.session.decodeErrorsTitle')"
        >
          <AlertTriangle class="h-3 w-3" :stroke-width="2" /> {{ decodeErrors }}
        </span>
        <HeaderButton :label="t('controller.session.disconnect')" @click="emit('disconnect')">
          <X class="h-4 w-4" :stroke-width="2" />
        </HeaderButton>
      </div>
    </header>

    <!-- Subscription usage (Claude + Codex), mirror of the desktop BudgetBadge.
         Kept above the run title so plan utilization is the first thing seen. -->
    <SessionPlanUsageBar
      :claude="claudeBudget"
      :codex="codexBudget"
      :claude-account="claudeAccount"
      :claude-accounts="claudeAccounts"
    />

    <!-- Run title -->
    <div class="shrink-0 px-3 py-1.5 border-b border-border/60">
      <p class="text-xs font-medium truncate">{{ title }}</p>
    </div>

    <!-- Another run is blocked waiting for a decision. One tap to go answer it. -->
    <button
      v-if="pendingElsewhere.length"
      class="shrink-0 w-full px-3 py-1.5 text-xs flex items-center justify-center gap-2 bg-warning/15 text-warning hover:bg-warning/25 transition-colors cursor-pointer"
      @click="emit('openInbox')"
    >
      <AlertTriangle class="h-3.5 w-3.5" :stroke-width="2" />
      {{ t('controller.browser.pendingElsewhere', pendingElsewhere.length) }}
    </button>

    <!-- Reconnect / error banner -->
    <div
      v-if="lastError && status !== 'connected'"
      class="shrink-0 px-3 py-1.5 text-xs text-center"
      :class="status === 'error' ? 'bg-destructive/15 text-destructive' : 'bg-warning/15 text-warning'"
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
    <div class="relative flex-1 min-h-0 flex flex-col">
      <main
        ref="mainEl"
        class="flex-1 min-h-0 overflow-auto px-3 py-3"
        @scroll="onScroll"
        @pointerdown="onStreamPointerDown"
      >
        <div ref="contentEl">
          <StreamLog :entries="entries" :running="running" :render-text="renderText" />
        </div>
      </main>
      <button
        v-if="!stickToBottom && entries.length"
        class="absolute bottom-3 left-1/2 -translate-x-1/2 inline-flex items-center gap-1 rounded-full border border-border bg-card px-3 py-1.5 text-xs font-medium text-foreground shadow-md hover:bg-accent transition-colors cursor-pointer"
        @click="jumpToNewest"
      >
        <ArrowDown class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('controller.session.newest') }}
      </button>
    </div>

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
        :allow-always="allowAlways"
        @decide="emit('decide', $event)"
        @answer="emit('answer', $event)"
      />
    </div>

    <!-- Composer -->
    <SessionComposer
      v-else
      :slash-commands="slashCommands"
      :project-files="projectFiles"
      :discovered-models="discoveredModels"
      :claude-accounts="claudeAccountChoices"
      :claude-account-id="claudeAccountId"
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
      @update:claude-account-id="emit('update:claudeAccountId', $event)"
      @request-files="emit('requestFiles')"
    />
  </div>
</template>
