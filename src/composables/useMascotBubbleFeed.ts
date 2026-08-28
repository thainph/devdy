// Aggregates the app's transient signals into the fox's speech bubble:
//   • live-run phase (busy / permission / done) → status chatter
//   • toasts (success / error / info) → mirrored into the bubble
// Calendar reminders push directly from CalendarReminder. Runs ONCE in the main
// window (CyberFoxHost), so it must not be instantiated more than once.
import { computed, watch, type Ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { invoke } from '@/lib/tauri'
import { useToast } from '@/composables/useToast'
import { useMascotBubble, type MascotBubbleVariant } from '@/composables/useMascotBubble'
import { pickMascotVoice } from '@/composables/mascotVoiceLines'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useLiveRunsStore, type LiveSession } from '@/stores/liveRuns'
import type { StreamEntry } from '@/lib/streamEvents'
import type { CyberFoxState } from '@/composables/useMascotState'

// Collapse the fine-grained phase into the few states worth "saying". thinking
// and loading both read as "busy" so the bubble doesn't flicker between them
// while a run streams.
type Coarse = 'idle' | 'busy' | 'permission' | 'success' | 'error'
type ImportantCoarse = 'permission' | 'success' | 'error'

const AGENT_SPEECH_TIMEOUT_MS = 9_000
const CONTEXT_CHARS = 3_600
const SPEECH_QUEUE_GAP_MS = 1_200
const MAX_SPEECH_QUEUE_ITEMS = 8

interface MascotSpeechJob {
  id: string
  phase: ImportantCoarse
  variant: MascotBubbleVariant
  fallbackKey: string
  context: string
  priority: number
  shouldSkip?: () => boolean
}

function coarsePhaseOf(state: CyberFoxState): Coarse {
  switch (state) {
    case 'permission':
      return 'permission'
    case 'thinking':
    case 'loading':
      return 'busy'
    case 'success':
      return 'success'
    case 'error':
      return 'error'
    default:
      return 'idle'
  }
}

function shorten(text: string, max = CONTEXT_CHARS): string {
  const normalized = text.replace(/\s+\n/g, '\n').replace(/\n{3,}/g, '\n\n').trim()
  if (normalized.length <= max) return normalized
  return `${normalized.slice(0, max - 3).trimEnd()}...`
}

function formatUnknown(value: unknown, max = 1_200): string {
  if (typeof value === 'string') return shorten(value, max)
  try {
    return shorten(JSON.stringify(value, null, 2) ?? String(value), max)
  } catch {
    return shorten(String(value), max)
  }
}

function latestResult(session: LiveSession): Extract<StreamEntry, { kind: 'result' }> | null {
  for (let i = session.entries.length - 1; i >= 0; i--) {
    const e = session.entries[i]
    if (e.kind === 'result') return e
  }
  return null
}

function latestResultIndex(session: LiveSession): number {
  for (let i = session.entries.length - 1; i >= 0; i--) {
    if (session.entries[i].kind === 'result') return i
  }
  return -1
}

function latestAssistantText(session: LiveSession): string {
  for (let i = session.entries.length - 1; i >= 0; i--) {
    const e = session.entries[i]
    if (e.kind === 'text' && e.text.trim()) return e.text.trim()
  }
  return ''
}

function latestErrorText(session: LiveSession): string {
  for (let i = session.entries.length - 1; i >= 0; i--) {
    const e = session.entries[i]
    if (e.kind === 'error' && e.text.trim()) return e.text.trim()
    if (e.kind === 'tool' && e.result?.is_error && e.result.content.trim()) {
      return `${e.displayName || e.name}: ${e.result.content.trim()}`
    }
    if (e.kind === 'result' && e.is_error && e.text.trim()) return e.text.trim()
  }
  return ''
}

function latestToolResult(session: LiveSession): string {
  for (let i = session.entries.length - 1; i >= 0; i--) {
    const e = session.entries[i]
    if (e.kind === 'tool' && e.result?.content.trim()) {
      const label = e.displayName || e.name
      return `${label}: ${e.result.content.trim()}`
    }
  }
  return ''
}

function buildMascotContext(phase: ImportantCoarse, session: LiveSession | null): string {
  if (!session) return ''
  const lines: string[] = [
    `Run status: ${session.status}`,
    session.model ? `Model: ${session.model}` : '',
  ].filter(Boolean)

  if (phase === 'permission') {
    const req = session.permissionQueue[0]
    if (!req) return ''
    lines.push('Event: permission request')
    lines.push(`Tool: ${req.display_name || req.tool_name}`)
    if (req.title) lines.push(`Title: ${req.title}`)
    if (req.description) lines.push(`Description: ${req.description}`)
    lines.push(`Input: ${formatUnknown(req.tool_input, 1_400)}`)
    return shorten(lines.join('\n'), CONTEXT_CHARS)
  }

  const result = latestResult(session)
  lines.push(phase === 'success' ? 'Event: run completed' : 'Event: run failed or stopped')
  if (result) {
    const meta = [
      result.duration_ms ? `duration ${(result.duration_ms / 1000).toFixed(1)}s` : '',
      result.num_turns ? `${result.num_turns} turns` : '',
      result.total_tokens ? `${result.total_tokens} tokens` : '',
    ].filter(Boolean)
    lines.push(`Result: ${result.is_error ? 'error' : 'success'}${meta.length ? ` (${meta.join(', ')})` : ''}`)
    if (result.text.trim()) lines.push(`Result text:\n${shorten(result.text, 1_600)}`)
  }

  const finalText = latestAssistantText(session)
  if (finalText) lines.push(`Latest assistant answer:\n${shorten(finalText, 1_800)}`)

  const errorText = latestErrorText(session)
  if (phase === 'error' && errorText) lines.push(`Error detail:\n${shorten(errorText, 1_200)}`)

  if (!finalText && phase === 'success') {
    const toolText = latestToolResult(session)
    if (toolText) lines.push(`Latest tool result:\n${shorten(toolText, 1_200)}`)
  }

  return shorten(lines.join('\n\n'), CONTEXT_CHARS)
}

async function invokeMascotSpeech(context: string, variant: MascotBubbleVariant, locale: string): Promise<string> {
  let timer: ReturnType<typeof setTimeout> | null = null
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error('Mascot speech timed out')), AGENT_SPEECH_TIMEOUT_MS)
  })
  try {
    return await Promise.race([
      invoke<string>('mascot_speak', { context, variant, locale }),
      timeout,
    ])
  } finally {
    if (timer) clearTimeout(timer)
  }
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

// Cap the "already handled" bookkeeping sets so a long-lived session with many
// runs / permission prompts can't leak memory. We only need recent history to
// avoid re-speaking the same event.
const MAX_SEEN = 200
function remember(set: Set<string>, id: string) {
  set.add(id)
  if (set.size > MAX_SEEN) {
    const oldest = set.values().next().value
    if (oldest !== undefined) set.delete(oldest)
  }
}

export function useMascotBubbleFeed(
  displayState: Ref<CyberFoxState>,
  activeSession: Ref<LiveSession | null>,
  enabled: Ref<boolean>,
) {
  const { t, locale } = useI18n()
  const { state: toastState } = useToast()
  const { push } = useMascotBubble()
  const appSettings = useAppSettingsStore()

  // --- live-run phase → status chatter ---------------------------------
  const coarse = computed(() => coarsePhaseOf(displayState.value))
  const agentSpeechEnabled = computed(() => appSettings.settings?.mascot_speech_enabled === 'true')

  // Say a canned phase line, preferring a recorded voice take (clip + matching
  // words) so the bubble text mirrors what the fox actually says; fall back to
  // the generic i18n string when no clip is installed for that variant.
  function sayPhase(variant: MascotBubbleVariant, fallbackKey: string) {
    if (!enabled.value) return
    const line = pickMascotVoice(variant, String(locale.value))
    push(line?.text ?? t(fallbackKey), variant)
  }

  const live = useLiveRunsStore()
  const speechQueue: MascotSpeechJob[] = []
  const queuedSpeechIds = new Set<string>()
  const seenDoneSignals = new Set<string>()
  const seenPermissionRequests = new Set<string>()
  let processingSpeechQueue = false

  async function speakJob(job: MascotSpeechJob) {
    if (!agentSpeechEnabled.value) {
      sayPhase(job.variant, job.fallbackKey)
      return
    }
    if (!job.context) {
      sayPhase(job.variant, job.fallbackKey)
      return
    }
    try {
      const text = (await invokeMascotSpeech(job.context, job.variant, String(locale.value))).trim()
      if (text) push(text, job.variant)
      else sayPhase(job.variant, job.fallbackKey)
    } catch (e) {
      console.warn('[mascot] agent speech failed; falling back to canned line', e)
      void invoke('cancel_mascot_speak').catch(() => {})
      sayPhase(job.variant, job.fallbackKey)
    }
  }

  async function processSpeechQueue() {
    if (processingSpeechQueue) return
    processingSpeechQueue = true
    try {
      while (speechQueue.length) {
        const job = speechQueue.shift()!
        queuedSpeechIds.delete(job.id)
        if (job.shouldSkip?.()) continue
        await speakJob(job)
        if (speechQueue.length) await wait(SPEECH_QUEUE_GAP_MS)
      }
    } finally {
      processingSpeechQueue = false
    }
  }

  function enqueueSpeechJob(job: MascotSpeechJob) {
    // Mascot turned off → never generate speech (no backend LLM call, no bubble).
    if (!enabled.value) return
    if (queuedSpeechIds.has(job.id)) return
    queuedSpeechIds.add(job.id)
    if (job.priority > 0) speechQueue.unshift(job)
    else speechQueue.push(job)
    while (speechQueue.length > MAX_SPEECH_QUEUE_ITEMS) {
      const dropped = speechQueue.pop()
      if (dropped) queuedSpeechIds.delete(dropped.id)
    }
    void processSpeechQueue()
  }

  function enqueuePermissionJob(
    session: LiveSession | null = activeSession.value,
    req = session?.permissionQueue[0],
  ) {
    if (!session || !req) {
      sayPhase('permission', 'mascot.bubble.permission')
      return
    }
    const id = `permission:${req.request_id}`
    if (seenPermissionRequests.has(id)) return
    remember(seenPermissionRequests, id)
    const context = buildMascotContext('permission', session)
    enqueueSpeechJob({
      id,
      phase: 'permission',
      variant: 'permission',
      fallbackKey: 'mascot.bubble.permission',
      context,
      priority: 10,
      shouldSkip: () => !session.permissionQueue.some((item) => item.request_id === req.request_id),
    })
  }

  watch(coarse, (phase, prev) => {
    if (phase === prev) return
    switch (phase) {
      case 'busy':
        sayPhase('thinking', 'mascot.bubble.busy')
        break
      case 'permission':
        enqueuePermissionJob()
        break
      case 'success':
      case 'error':
        break
      // idle → say nothing (let the last bubble auto-hide)
    }
  })

  watch(
    () =>
      Array.from(live.sessions.values()).map((session) => ({
        runId: session.runId,
        status: session.status,
        notifyDone: session.notifyDone,
        resultIndex: latestResultIndex(session),
      })),
    () => {
      live.sessions.forEach((session) => {
        if (!session.notifyDone || session.status === 'running') return
        const resultIndex = latestResultIndex(session)
        const id = `done:${session.runId}:${session.status}:${resultIndex}`
        if (seenDoneSignals.has(id)) return
        remember(seenDoneSignals, id)
        const phase: ImportantCoarse =
          session.status === 'failed' || session.status === 'cancelled' ? 'error' : 'success'
        enqueueSpeechJob({
          id,
          phase,
          variant: phase,
          fallbackKey: phase === 'error' ? 'mascot.bubble.error' : 'mascot.bubble.success',
          context: buildMascotContext(phase, session),
          priority: 0,
        })
      })
    },
    { immediate: true },
  )

  watch(
    () =>
      Array.from(live.sessions.values()).flatMap((session) =>
        session.permissionQueue.map((req) => `${session.runId}:${req.request_id}`),
      ),
    () => {
      live.sessions.forEach((session) => {
        for (const req of session.permissionQueue) enqueuePermissionJob(session, req)
      })
    },
    { immediate: true },
  )

  // --- toasts → mirror into the bubble ---------------------------------
  const TOAST_VARIANT: Record<string, MascotBubbleVariant> = {
    success: 'success',
    error: 'error',
    info: 'info',
  }
  let lastToastId = 0
  watch(
    () => toastState.items.map((i) => i.id),
    () => {
      // Mirror new toasts into the fox bubble. We always advance lastToastId so
      // toggling the mascot on later never replays a backlog of old toasts.
      for (const item of toastState.items) {
        if (item.id <= lastToastId) continue
        lastToastId = item.id
        if (!enabled.value) continue
        const variant = TOAST_VARIANT[item.variant] ?? 'info'
        // Don't let the fox narrate system error toasts — they're often noisy
        // technical dumps and spammed the bubble/TTS.
        if (variant === 'error') continue
        push(item.message, variant)
      }
    },
    { deep: true },
  )
}
