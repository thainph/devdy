// Shared "brain" of the DY Cyber Fox mascot. Derives the single phase the fox
// should show (idle / thinking / loading / success / error / permission) from
// all live sessions, plus how many light streams to orbit and the user's
// enabled/size preferences. Both the in-app floating mascot (CyberFoxFloating)
// and the desktop-pet bridge (CyberFoxHost → MascotWindow) consume this so the
// two rendering surfaces always agree.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useLiveRunsStore, type LiveSession } from '@/stores/liveRuns'
import { useMascotLevelStore } from '@/stores/mascotLevel'

export type CyberFoxState =
  | 'idle'
  | 'thinking'
  | 'loading'
  | 'success'
  | 'error'
  | 'permission'

export type CyberFoxSize = 'sm' | 'md' | 'lg'
export type CyberFoxMode = 'in-app' | 'desktop'

// Attention priority: the fox reflects the most important phase across all live
// sessions. Higher wins so a running tool outshines a quiet think, and a pending
// permission outshines everything.
const PHASE_PRIORITY: Record<CyberFoxState, number> = {
  permission: 5,
  loading: 4,
  thinking: 3,
  idle: 0,
  // transient-only states never come from a session phase
  success: 0,
  error: 0,
}

/** Derive the current phase of a single session from its live stream. */
function sessionPhase(session: LiveSession): CyberFoxState {
  // A pending prompt — tool permission OR AskUserQuestion — needs the user.
  if (session.permissionQueue.length > 0) return 'permission'
  // No active run for this session → resting.
  if (session.status !== 'running') return 'idle'
  // Turn accepted but nothing streamed back yet — the model is thinking before output.
  if (!session.hasStreamEvents) return 'thinking'
  // Classify by the latest meaningful stream activity (skip logs/errors/results).
  for (let i = session.entries.length - 1; i >= 0; i--) {
    const e = session.entries[i]
    // A tool entry means the fox is actively working — whether the tool is still
    // running (no result yet) or its result just landed in place. We deliberately
    // do NOT flip to 'thinking' when a result attaches: the tool_result is folded
    // back onto the SAME entry (streamEvents), so a multi-tool turn would otherwise
    // flicker loading→thinking→loading between every tool call. Genuine reasoning
    // is surfaced by dedicated 'thinking' entries below.
    if (e.kind === 'tool') return 'loading'
    if (e.kind === 'text') return 'loading' // streaming text output
    if (e.kind === 'thinking') return 'thinking' // reasoning, no text yet
  }
  return 'thinking'
}

/**
 * Reactive mascot state, decoupled from any particular rendering surface.
 * `enabled`, `mode` and `mascotSize` come from settings; `displayState` and
 * `runningCount` come from the live-runs store (with a short success/error
 * flash after a run finishes).
 */
// Detected once per session: is this a weak machine? Few logical cores or low
// reported RAM → auto-enable lite mode unless the user set it explicitly.
const autoLiteMachine = (() => {
  const cores = typeof navigator !== 'undefined' ? navigator.hardwareConcurrency || 8 : 8
  const mem = typeof navigator !== 'undefined' ? (navigator as { deviceMemory?: number }).deviceMemory : undefined
  return cores <= 4 || (typeof mem === 'number' && mem <= 4)
})()

export function useMascotState() {
  const appSettings = useAppSettingsStore()
  const live = useLiveRunsStore()
  const levelStore = useMascotLevelStore()
  // Kick off the first cumulative-usage read so the fox shows its earned realm.
  levelStore.ensureLoaded()

  const enabled = computed(() => appSettings.settings?.cyber_fox_enabled === 'true')
  const soundEnabled = computed(() => appSettings.settings?.cyber_fox_sound !== 'false')
  // Lite / performance mode. 'true'/'false' are explicit user choices; anything
  // else ('auto' or unset) falls back to the auto weak-machine detection above.
  const liteMode = computed(() => {
    const v = appSettings.settings?.cyber_fox_lite
    if (v === 'true') return true
    if (v === 'false') return false
    return autoLiteMachine
  })
  // TTS voice preferences (per-language name + shared rate + pitch) from settings.
  const voiceNameVi = computed(() => appSettings.settings?.cyber_fox_voice_vi ?? '')
  const voiceNameEn = computed(() => appSettings.settings?.cyber_fox_voice_en ?? '')
  const voiceRate = computed(() => {
    const r = parseFloat(appSettings.settings?.cyber_fox_voice_rate ?? '')
    return Number.isFinite(r) ? r : 1
  })
  const voicePitch = computed(() => {
    const p = parseFloat(appSettings.settings?.cyber_fox_voice_pitch ?? '')
    return Number.isFinite(p) ? p : 1.15
  })
  const mode = computed<CyberFoxMode>(() =>
    appSettings.settings?.cyber_fox_mode === 'desktop' ? 'desktop' : 'in-app',
  )
  const mascotSize = computed<CyberFoxSize>(() => {
    const size = appSettings.settings?.cyber_fox_size
    return size === 'sm' || size === 'lg' ? size : 'md'
  })

  const steadyFocus = computed<{ state: CyberFoxState; session: LiveSession | null }>(() => {
    let best: CyberFoxState = 'idle'
    let bestSession: LiveSession | null = null
    live.sessions.forEach((session) => {
      const phase = sessionPhase(session)
      if (PHASE_PRIORITY[phase] > PHASE_PRIORITY[best]) {
        best = phase
        bestSession = session
      }
    })
    return { state: best, session: bestSession }
  })
  const steadyState = computed<CyberFoxState>(() => steadyFocus.value.state)

  // Transient success/error flash shown for a moment once a run finishes.
  // Bounded so long-lived sessions with many runs don't leak memory: we only
  // need to remember recent signals to avoid re-flashing the same completion.
  const MAX_SHOWN_SIGNALS = 200
  const shownDoneSignals = new Set<string>()
  const rememberSignal = (id: string) => {
    shownDoneSignals.add(id)
    if (shownDoneSignals.size > MAX_SHOWN_SIGNALS) {
      const oldest = shownDoneSignals.values().next().value
      if (oldest !== undefined) shownDoneSignals.delete(oldest)
    }
  }
  const transientState = ref<CyberFoxState | null>(null)
  const transientRunId = ref<string | null>(null)
  let transientTimer: ReturnType<typeof setTimeout> | null = null

  const doneSignal = computed(() => {
    for (const session of live.sessions.values()) {
      if (!session.notifyDone || session.status === 'running') continue
      return `${session.runId}:${session.status}`
    }
    return ''
  })

  watch(
    doneSignal,
    (signal) => {
      if (!signal || shownDoneSignals.has(signal)) return
      rememberSignal(signal)
      const sep = signal.lastIndexOf(':')
      const status = signal.slice(sep + 1)
      transientRunId.value = signal.slice(0, sep)
      transientState.value = status === 'failed' || status === 'cancelled' ? 'error' : 'success'
      if (transientTimer) clearTimeout(transientTimer)
      transientTimer = setTimeout(() => {
        transientState.value = null
        transientRunId.value = null
        transientTimer = null
      }, 1400)
      // A run just finished → its token usage is now in the ledger; re-read the
      // cumulative total so the fox can break through to a new realm/tier.
      levelStore.refresh()
    },
    { immediate: true },
  )

  const displayState = computed<CyberFoxState>(() => {
    if (steadyState.value === 'permission') return 'permission'
    return transientState.value ?? steadyState.value
  })

  const activeSession = computed<LiveSession | null>(() => {
    if (transientState.value && transientRunId.value) {
      return live.sessions.get(transientRunId.value) ?? null
    }
    return steadyFocus.value.session
  })

  // One orbiting light stream per run that is actively running.
  const runningCount = computed(() => {
    let n = 0
    live.sessions.forEach((s) => {
      if (s.status === 'running') n++
    })
    return Math.max(1, n)
  })

  // Earned evolution — realm + tier derived from cumulative token usage.
  const mascotLevel = computed(() => levelStore.info)
  const evolutionRealm = computed(() => levelStore.info.realmId)
  const evolutionTier = computed(() => levelStore.info.tier)
  // Ascension stars ⭐ — one per completed 35-level cycle (0 on the first cycle).
  const mascotStars = computed(() => levelStore.info.stars)

  onBeforeUnmount(() => {
    if (transientTimer) clearTimeout(transientTimer)
  })

  return {
    enabled,
    soundEnabled,
    liteMode,
    mode,
    mascotSize,
    voiceNameVi,
    voiceNameEn,
    voiceRate,
    voicePitch,
    displayState,
    activeSession,
    runningCount,
    mascotLevel,
    evolutionRealm,
    evolutionTier,
    mascotStars,
  }
}
