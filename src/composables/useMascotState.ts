// Shared "brain" of the DY Cyber Fox mascot. Derives the single phase the fox
// should show (idle / thinking / loading / success / error / permission) from
// all live sessions, plus how many light streams to orbit and the user's
// enabled/size preferences. Both the in-app floating mascot (CyberFoxFloating)
// and the desktop-pet bridge (CyberFoxHost → MascotWindow) consume this so the
// two rendering surfaces always agree.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useLiveRunsStore, type LiveSession } from '@/stores/liveRuns'

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
    if (e.kind === 'tool') return e.result ? 'thinking' : 'loading' // running a tool → loading
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
export function useMascotState() {
  const appSettings = useAppSettingsStore()
  const live = useLiveRunsStore()

  const enabled = computed(() => appSettings.settings?.cyber_fox_enabled === 'true')
  const soundEnabled = computed(() => appSettings.settings?.cyber_fox_sound !== 'false')
  // Lite / performance mode: opt-in effect trimming for weaker machines.
  const liteMode = computed(() => appSettings.settings?.cyber_fox_lite === 'true')
  const mode = computed<CyberFoxMode>(() =>
    appSettings.settings?.cyber_fox_mode === 'desktop' ? 'desktop' : 'in-app',
  )
  const mascotSize = computed<CyberFoxSize>(() => {
    const size = appSettings.settings?.cyber_fox_size
    return size === 'sm' || size === 'lg' ? size : 'md'
  })

  const steadyState = computed<CyberFoxState>(() => {
    let best: CyberFoxState = 'idle'
    live.sessions.forEach((session) => {
      const phase = sessionPhase(session)
      if (PHASE_PRIORITY[phase] > PHASE_PRIORITY[best]) best = phase
    })
    return best
  })

  // Transient success/error flash shown for a moment once a run finishes.
  const shownDoneSignals = new Set<string>()
  const transientState = ref<CyberFoxState | null>(null)
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
      shownDoneSignals.add(signal)
      const status = signal.slice(signal.lastIndexOf(':') + 1)
      transientState.value = status === 'failed' || status === 'cancelled' ? 'error' : 'success'
      if (transientTimer) clearTimeout(transientTimer)
      transientTimer = setTimeout(() => {
        transientState.value = null
        transientTimer = null
      }, 1400)
    },
    { immediate: true },
  )

  const displayState = computed<CyberFoxState>(() => {
    if (steadyState.value === 'permission') return 'permission'
    return transientState.value ?? steadyState.value
  })

  // One orbiting light stream per run that is actively running.
  const runningCount = computed(() => {
    let n = 0
    live.sessions.forEach((s) => {
      if (s.status === 'running') n++
    })
    return Math.max(1, n)
  })

  onBeforeUnmount(() => {
    if (transientTimer) clearTimeout(transientTimer)
  })

  return { enabled, soundEnabled, liteMode, mode, mascotSize, displayState, runningCount }
}
