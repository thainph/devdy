import { defineStore } from 'pinia'
import { reactive, ref } from 'vue'
import { useRunsStore } from './runs'
import { useLiveRunsStore } from './liveRuns'
import { parseStreamLog, entriesToPlainText, type StreamEntry } from '@/lib/streamEvents'
import {
  buildOpening,
  buildBOpening,
  buildRelay,
  hasConsensus,
  type DuoPromptConfig,
} from '@/lib/duoPrompts'

/**
 * Duo Session orchestrator — drives two persistent AI runs (a "designer" and a
 * "reviewer") that critique each other in a ping-pong loop until they converge,
 * hit a round cap, or the user stops.
 *
 * Design decisions (per the feature plan):
 *  - Orchestration lives in the frontend (this store) and reuses the existing
 *    `runs` (Tauri invokes) + `liveRuns` (streaming state + `run:done` hook).
 *  - Each side is a PERSISTENT run: it keeps its own conversation/context across
 *    rounds and only receives the OTHER side's latest reply as a follow-up. We
 *    snapshot `entries.length` before each turn so we relay just that turn's new
 *    assistant text, not the whole transcript.
 *  - Stop conditions: max rounds, a reviewer consensus token, or manual stop.
 */

export type DuoRole = 'designer' | 'reviewer'

export type DuoPhase =
  | 'idle'
  | 'starting'
  | 'waitingDesigner'
  | 'waitingReviewer'
  | 'done' // reviewer approved
  | 'stopped' // manual stop or round cap
  | 'error'

/** One completed turn, captured for the timeline UI. */
export interface DuoTurn {
  role: DuoRole
  engine: string
  round: number
  text: string
}

/** How the two sessions are sourced. */
export type DuoSourceMode = 'new' | 'existing'

/** Everything the caller must supply to start a duo. */
export interface DuoStartConfig {
  projectId: string
  /** 'new' creates two fresh session runs; 'existing' reuses runs already in
   * the project (their first turn is a resume + follow-up). */
  mode: DuoSourceMode
  designerEngine: string
  reviewerEngine: string
  designerModel?: string
  reviewerModel?: string
  /** Required when mode === 'existing'. */
  designerRunId?: string
  reviewerRunId?: string
  /** Display names + role instructions for each side (generic — not tied to
   * design/review). Blank labels fall back to `A (engine)` / `B (engine)`. */
  designerLabel?: string
  reviewerLabel?: string
  designerInstruction?: string
  reviewerInstruction?: string
  permissionMode?: string
  goal: string
  maxRounds: number
  /** Optional — blank means no consensus stop (rely on max rounds / manual). */
  consensusToken: string
  overrideBudget?: boolean
}

interface DuoState {
  /** Stable id of this duo (kept across turns; identifies the history record). */
  id: string
  /** ISO timestamp of the last persisted change (for history ordering). */
  savedAt: string
  active: boolean
  phase: DuoPhase
  mode: DuoSourceMode
  projectId: string
  designerEngine: string
  reviewerEngine: string
  designerModel: string
  reviewerModel: string
  permissionMode: string
  overrideBudget: boolean
  designerLabel: string
  reviewerLabel: string
  designerInstruction: string
  reviewerInstruction: string
  designerRunId: string | null
  reviewerRunId: string | null
  /** Number of FULL rounds completed (each = designer turn + reviewer turn). */
  round: number
  maxRounds: number
  consensusToken: string
  goal: string
  waiting: DuoRole | null
  stopReason: string | null
  error: string | null
  turns: DuoTurn[]
  /** True when the loop is paused (manual stop / error / round cap) and can be
   * resumed from the last completed turn via {@link OrchestratorStore.continue}. */
  resumable: boolean
}

function initialState(): DuoState {
  return {
    id: '',
    savedAt: '',
    active: false,
    phase: 'idle',
    mode: 'new',
    projectId: '',
    designerEngine: 'claude',
    reviewerEngine: 'codex',
    designerModel: '',
    reviewerModel: '',
    permissionMode: '',
    overrideBudget: false,
    designerLabel: 'A',
    reviewerLabel: 'B',
    designerInstruction: '',
    reviewerInstruction: '',
    designerRunId: null,
    reviewerRunId: null,
    round: 0,
    maxRounds: 6,
    consensusToken: '[APPROVED]',
    goal: '',
    waiting: null,
    stopReason: null,
    error: null,
    turns: [],
    resumable: false,
  }
}

export const useOrchestratorStore = defineStore('orchestrator', () => {
  const runs = useRunsStore()
  const live = useLiveRunsStore()

  const state = reactive<DuoState>(initialState())

  // Non-reactive side data (models/permission/budget now live in `state` so a
  // whole duo can be serialized to history in one shot).
  let designerStarted = false
  let reviewerStarted = false
  const turnStartLen = new Map<string, number>()
  let unsubs: Array<() => void> = []

  // ── History persistence (localStorage) ──────────────────────────────────────
  const HISTORY_KEY = 'devdy.duoHistory'
  const HISTORY_MAX = 30
  const history = reactive<DuoState[]>(loadHistory())

  function loadHistory(): DuoState[] {
    try {
      const raw = localStorage.getItem(HISTORY_KEY)
      const arr = raw ? JSON.parse(raw) : []
      return Array.isArray(arr) ? arr : []
    } catch {
      return []
    }
  }

  function saveHistory() {
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify(history))
    } catch {
      /* storage unavailable — in-memory history still works this session */
    }
  }

  function nowIso(): string {
    return new Date().toISOString()
  }

  function newId(): string {
    try {
      return crypto.randomUUID()
    } catch {
      return `duo-${Date.now()}-${Math.floor(Math.random() * 1e6)}`
    }
  }

  /** Upsert the current duo into the persisted history (by id). */
  function persist() {
    if (!state.id) return
    state.savedAt = nowIso()
    const snap = JSON.parse(JSON.stringify(state)) as DuoState
    const idx = history.findIndex((h) => h.id === state.id)
    if (idx >= 0) history[idx] = snap
    else history.unshift(snap)
    // Newest first; cap the list.
    history.sort((a, b) => (b.savedAt || '').localeCompare(a.savedAt || ''))
    if (history.length > HISTORY_MAX) history.splice(HISTORY_MAX)
    saveHistory()
  }

  function promptCfg(): DuoPromptConfig {
    return {
      goal: state.goal,
      aLabel: state.designerLabel,
      bLabel: state.reviewerLabel,
      aInstruction: state.designerInstruction,
      bInstruction: state.reviewerInstruction,
      consensusToken: state.consensusToken,
    }
  }

  /** Pull the last assistant text of a turn out of a StreamEntry list (the text
   * emitted after the most recent `user` entry). */
  function lastReplyFromEntries(entries: StreamEntry[]): string {
    let start = 0
    for (let i = entries.length - 1; i >= 0; i--) {
      if (entries[i].kind === 'user') { start = i + 1; break }
    }
    return entries
      .slice(start)
      .filter((e: StreamEntry): e is Extract<StreamEntry, { kind: 'text' }> => e.kind === 'text')
      .map((e) => e.text)
      .join('\n\n')
      .trim()
  }

  /**
   * Read an EXISTING run's last output from its transcript, WITHOUT re-running
   * it. Used to seed "existing" mode: session A's final message is forwarded to
   * B as-is. Best-effort with fallbacks so it works across engines/log formats:
   *   1. live session entries (last assistant text)
   *   2. parsed stream log (last assistant text, else whole conversation)
   *   3. raw log text (Codex / non-stream logs that don't parse)
   * Returns '' only when there is genuinely nothing on record.
   */
  async function loadLastReply(runId: string): Promise<string> {
    const live0 = live.get(runId)
    if (live0 && live0.entries.length) {
      const fromLive = lastReplyFromEntries(live0.entries)
      if (fromLive) return fromLive
    }
    let log = ''
    try { log = await runs.getRunLog(runId) } catch { return '' }
    if (!log.trim()) return ''
    const parsed = parseStreamLog(log)
    if (parsed && parsed.entries.length) {
      const last = lastReplyFromEntries(parsed.entries)
      if (last) return last
      // Stream log without a trailing assistant text — hand over the whole
      // rendered conversation rather than nothing.
      const whole = entriesToPlainText(parsed.entries).trim()
      if (whole) return whole
    }
    // Non-stream / raw log (e.g. Codex): forward the raw transcript text.
    return log.trim()
  }

  /** Extract only the assistant text produced during the run's latest turn. */
  function extractLatestReply(runId: string): string {
    const s = live.get(runId)
    if (!s) return ''
    const start = turnStartLen.get(runId) ?? 0
    return s.entries
      .slice(start)
      .filter((e: StreamEntry): e is Extract<StreamEntry, { kind: 'text' }> => e.kind === 'text')
      .map((e) => e.text)
      .join('\n\n')
      .trim()
  }

  /** Start a brand-new turn (first message) on a not-yet-run session. */
  async function startTurn(runId: string, engine: string, model: string | undefined, text: string) {
    live.reset(runId, state.projectId)
    live.pushUser(runId, state.projectId, text)
    turnStartLen.set(runId, live.get(runId)?.entries.length ?? 0)
    await live.startListening(runId, state.projectId)
    await runs.startRun(runId, engine, state.permissionMode || undefined, text, model || undefined, [], state.overrideBudget)
  }

  /**
   * Continue an EXISTING session for its first turn in the duo. The picked run's
   * process is usually dead, so we resume it (restoring its own on-disk context)
   * before sending the follow-up. A session that happens to still be running
   * skips the resume. Subsequent turns use {@link relayTurn}.
   */
  async function continueTurn(runId: string, model: string | undefined, text: string) {
    const wasRunning = runs.runs.find((r) => r.id === runId)?.status === 'running'
    const s = live.ensure(runId, state.projectId)
    live.pushUser(runId, state.projectId, text)
    turnStartLen.set(runId, s.entries.length)
    live.setStatus(runId, 'running')
    await live.startListening(runId, state.projectId)
    if (!wasRunning) {
      await runs.resumeRun(runId, state.permissionMode || undefined, model || undefined, state.overrideBudget)
    }
    await runs.sendUserMessage(runId, text, [], state.overrideBudget)
  }

  /** Dispatch the FIRST turn of a side by source mode. */
  async function firstTurn(runId: string, engine: string, model: string | undefined, text: string) {
    if (state.mode === 'existing') await continueTurn(runId, model, text)
    else await startTurn(runId, engine, model, text)
  }

  /**
   * Send a follow-up turn. Each turn's CLI process EXITS on `run:done`, so the
   * next turn must resume the session before sending (otherwise the backend
   * rejects with "run not active"). {@link continueTurn} already resumes when
   * the run isn't currently live, so a relay is just a continue with no model
   * override (the session keeps its own model).
   */
  async function relayTurn(runId: string, text: string) {
    await continueTurn(runId, undefined, text)
  }

  /** (Re)attach the `run:done` subscriptions for both sessions. */
  function subscribe() {
    teardown()
    unsubs.push(live.onDone(state.designerRunId!, (s) => { void onDesignerDone(s) }))
    unsubs.push(live.onDone(state.reviewerRunId!, (s) => { void onReviewerDone(s) }))
  }

  /**
   * Dispatch one side's turn. Sets the waiting phase, records the step for a
   * possible resume, and picks first-turn-vs-relay from the per-side "started"
   * flag (set only AFTER a first turn succeeds, so an early failure retries as a
   * first turn rather than a resume with no session id).
   */
  async function dispatch(role: DuoRole, text: string) {
    const runId = role === 'designer' ? state.designerRunId! : state.reviewerRunId!
    const engine = role === 'designer' ? state.designerEngine : state.reviewerEngine
    const model = (role === 'designer' ? state.designerModel : state.reviewerModel) || undefined
    state.waiting = role
    state.phase = role === 'designer' ? 'waitingDesigner' : 'waitingReviewer'
    const started = role === 'designer' ? designerStarted : reviewerStarted
    if (started) {
      await relayTurn(runId, text)
    } else {
      await firstTurn(runId, engine, model, text)
      if (role === 'designer') designerStarted = true
      else reviewerStarted = true
    }
  }

  /**
   * Issue the next turn implied by the last COMPLETED turn. Because a turn is
   * only recorded in `turns` after it finishes, the "next" step is exactly the
   * one that was pending/failed — so this both drives the normal loop's resume
   * and the initial kick-off (no turns yet → designer seed).
   */
  async function pumpNext() {
    const cfg = promptCfg()
    // Drop any trailing empty turn(s) so a resume never relays blank text to the
    // other side — it retries the side that produced nothing instead.
    while (state.turns.length && !state.turns[state.turns.length - 1].text.trim()) {
      state.turns.pop()
    }
    const last = state.turns[state.turns.length - 1]

    // ── First step ──────────────────────────────────────────────────────────
    if (!last) {
      if (state.mode === 'existing') {
        // Existing sessions already carry their own context — just take A's last
        // output and forward it to B. A is NOT re-run.
        const aLast = await loadLastReply(state.designerRunId!)
        if (aLast) {
          designerStarted = true
          state.turns.push({ role: 'designer', engine: state.designerEngine, round: 1, text: aLast })
          await dispatch('reviewer', buildRelay(cfg, cfg.bLabel, cfg.aLabel, aLast))
          return
        }
        // Couldn't read A's output (empty/unreadable log) — fall back to running
        // A first (it still resumes its own context) instead of dead-ending.
      }
      await dispatch('designer', buildOpening(cfg))
      return
    }

    // ── Subsequent steps: relay the last completed turn to the other side ─────
    if (last.role === 'designer') {
      // B's very first turn only needs an opening in "new" mode; in "existing"
      // mode B already has context, so a plain relay is enough.
      const prompt = reviewerStarted || state.mode === 'existing'
        ? buildRelay(cfg, cfg.bLabel, cfg.aLabel, last.text)
        : buildBOpening(cfg, last.text)
      await dispatch('reviewer', prompt)
    } else {
      await dispatch('designer', buildRelay(cfg, cfg.aLabel, cfg.bLabel, last.text))
    }
  }

  function finish(phase: 'done' | 'stopped', reason: string) {
    state.phase = phase
    state.stopReason = reason
    state.waiting = null
    state.active = false
    // A round-cap stop is resumable (bump maxRounds + continue); consensus isn't.
    state.resumable = phase === 'stopped'
    teardown()
    persist()
  }

  function fail(message: string) {
    state.error = message
    state.phase = 'error'
    state.stopReason = 'error'
    state.active = false
    state.resumable = true
    // Best-effort: stop whichever session is mid-turn.
    const runningId = state.waiting === 'designer' ? state.designerRunId : state.reviewerRunId
    if (runningId) runs.cancelRun(runningId).catch(() => {})
    state.waiting = null
    teardown()
    persist()
  }

  function teardown() {
    for (const u of unsubs) {
      try { u() } catch { /* ignore */ }
    }
    unsubs = []
  }

  async function onDesignerDone(status: string) {
    if (!state.active || state.waiting !== 'designer') return
    try {
      if (status === 'failed' || status === 'cancelled') {
        fail(`Designer kết thúc với trạng thái: ${status}`)
        return
      }
      const reply = extractLatestReply(state.designerRunId!)
      if (!reply) {
        fail(`${state.designerLabel} không tạo ra nội dung nào ở lượt này. Hãy bấm Tiếp tục để thử lại.`)
        return
      }
      state.turns.push({ role: 'designer', engine: state.designerEngine, round: state.round + 1, text: reply })
      persist()

      // Either side may signal consensus and end the loop.
      if (hasConsensus(reply, state.consensusToken)) {
        finish('done', 'consensus')
        return
      }

      // Relay A's turn to B. B needs an opening only on its first turn in "new"
      // mode; in "existing" mode B already has context, so relay directly.
      const cfg = promptCfg()
      const prompt = reviewerStarted || state.mode === 'existing'
        ? buildRelay(cfg, cfg.bLabel, cfg.aLabel, reply)
        : buildBOpening(cfg, reply)
      await dispatch('reviewer', prompt)
    } catch (e) {
      fail(`Lỗi khi chuyển sang ${state.reviewerLabel}: ${String(e)}`)
    }
  }

  async function onReviewerDone(status: string) {
    if (!state.active || state.waiting !== 'reviewer') return
    try {
      if (status === 'failed' || status === 'cancelled') {
        fail(`Reviewer kết thúc với trạng thái: ${status}`)
        return
      }
      const reply = extractLatestReply(state.reviewerRunId!)
      if (!reply) {
        fail(`${state.reviewerLabel} không tạo ra nội dung nào ở lượt này. Hãy bấm Tiếp tục để thử lại.`)
        return
      }
      state.turns.push({ role: 'reviewer', engine: state.reviewerEngine, round: state.round + 1, text: reply })
      state.round += 1
      persist()

      if (hasConsensus(reply, state.consensusToken)) {
        finish('done', 'consensus')
        return
      }
      if (state.round >= state.maxRounds) {
        finish('stopped', 'maxRounds')
        return
      }

      // Relay B's turn back to A for another round.
      const cfg = promptCfg()
      await dispatch('designer', buildRelay(cfg, cfg.aLabel, cfg.bLabel, reply))
    } catch (e) {
      fail(`Lỗi khi chuyển về ${state.designerLabel}: ${String(e)}`)
    }
  }

  /** Kick off a new duo. Rejects (and leaves state clean) if setup fails. */
  async function start(cfg: DuoStartConfig) {
    if (state.active) throw new Error('Đã có một phiên Duo đang chạy')
    if (cfg.mode === 'existing') {
      if (!cfg.designerRunId || !cfg.reviewerRunId) throw new Error('Cần chọn đủ 2 session có sẵn')
      if (cfg.designerRunId === cfg.reviewerRunId) throw new Error('Hai session phải khác nhau')
    }
    reset()

    state.mode = cfg.mode
    state.projectId = cfg.projectId
    state.designerEngine = cfg.designerEngine
    state.reviewerEngine = cfg.reviewerEngine
    state.designerLabel = cfg.designerLabel?.trim() || `A (${cfg.designerEngine})`
    state.reviewerLabel = cfg.reviewerLabel?.trim() || `B (${cfg.reviewerEngine})`
    state.designerInstruction = cfg.designerInstruction?.trim() || ''
    state.reviewerInstruction = cfg.reviewerInstruction?.trim() || ''
    state.goal = cfg.goal
    state.maxRounds = Math.max(1, cfg.maxRounds)
    state.consensusToken = cfg.consensusToken.trim()
    state.designerModel = cfg.designerModel || ''
    state.reviewerModel = cfg.reviewerModel || ''
    state.permissionMode = cfg.permissionMode || ''
    state.overrideBudget = cfg.overrideBudget ?? false
    state.id = newId()

    state.phase = 'starting'
    try {
      let designerId: string
      let reviewerId: string
      if (cfg.mode === 'existing') {
        designerId = cfg.designerRunId!
        reviewerId = cfg.reviewerRunId!
      } else {
        const designer = await runs.createSessionRun(cfg.projectId, cfg.designerEngine)
        const reviewer = await runs.createSessionRun(cfg.projectId, cfg.reviewerEngine)
        designerId = designer.id
        reviewerId = reviewer.id
        // Surface both new runs in the sidebar list if this project is loaded.
        runs.fetchRuns(cfg.projectId).catch(() => {})
      }
      state.designerRunId = designerId
      state.reviewerRunId = reviewerId
      subscribe()

      state.active = true
      persist()
      // pumpNext seeds the right first step for the mode (new → A opens;
      // existing → forward A's last output to B).
      await pumpNext()
    } catch (e) {
      state.active = false
      state.phase = 'error'
      state.error = String(e)
      // Resumable only if both sessions exist (so continue has something to
      // re-dispatch); a setup failure before that leaves nothing to resume.
      state.resumable = !!state.designerRunId && !!state.reviewerRunId
      teardown()
      persist()
      throw e
    }
  }

  /** Manual stop — cancels the in-flight session and pauses the loop (resumable). */
  async function stop() {
    if (!state.active) return
    const runningId = state.waiting === 'designer' ? state.designerRunId : state.reviewerRunId
    state.stopReason = 'manual'
    state.phase = 'stopped'
    state.active = false
    state.waiting = null
    state.resumable = true
    teardown()
    persist()
    if (runningId) {
      try { await runs.cancelRun(runningId) } catch { /* best effort */ }
    }
  }

  /**
   * Resume a paused loop (after a manual stop, an error, or a round cap) from
   * the last completed turn. The next step is recomputed from `turns`, so it
   * re-issues exactly the turn that was pending/failed. Reads the CURRENT
   * `state.maxRounds`, so bumping it in the UI before continuing extends the run.
   */
  async function continueDuo() {
    if (state.active || !state.resumable) return
    if (!state.designerRunId || !state.reviewerRunId) return
    // If we already hit the cap, give the loop at least one more round.
    if (state.round >= state.maxRounds) state.maxRounds = state.round + 1
    state.error = null
    state.stopReason = null
    state.resumable = false
    subscribe()
    state.active = true
    persist()
    try {
      await pumpNext()
    } catch (e) {
      fail(`Lỗi khi tiếp tục: ${String(e)}`)
    }
  }

  /** Reset the ACTIVE duo back to idle (does not cancel — call stop() first if
   * active). History is untouched. */
  function reset() {
    teardown()
    turnStartLen.clear()
    designerStarted = false
    reviewerStarted = false
    Object.assign(state, initialState())
  }

  /** Repopulate the two streams from disk so a restored duo shows its history
   * (live entries are memory-only and gone after an app restart). Best-effort. */
  async function hydrateStreamsFromDisk() {
    for (const runId of [state.designerRunId, state.reviewerRunId]) {
      if (!runId) continue
      const s = live.ensure(runId, state.projectId)
      if (s.entries.length) continue
      try {
        const log = await runs.getRunLog(runId)
        const parsed = parseStreamLog(log)
        if (parsed && parsed.entries.length) {
          s.entries.push(...parsed.entries)
          s.hasStreamEvents = true
        }
      } catch { /* no transcript yet — leave the stream empty */ }
    }
  }

  /**
   * Load a saved duo from history into the live state so the user can inspect it
   * and Continue. The loop is NOT running after a load (any prior subprocess is
   * gone), so we present it as paused/resumable; Continue re-subscribes and
   * re-dispatches the pending step from `turns`.
   */
  async function restore(id: string) {
    const rec = history.find((h) => h.id === id)
    if (!rec) return
    reset()
    Object.assign(state, JSON.parse(JSON.stringify(rec)) as DuoState)
    // Nothing is actually running after a load.
    state.active = false
    state.waiting = null
    designerStarted = state.turns.some((t) => t.role === 'designer')
    reviewerStarted = state.turns.some((t) => t.role === 'reviewer')
    // Present an interrupted/mid-run save as paused so Continue is offered.
    if (state.phase === 'waitingDesigner' || state.phase === 'waitingReviewer' || state.phase === 'starting') {
      state.phase = 'stopped'
      state.stopReason = 'manual'
    }
    state.resumable = state.phase !== 'done' && !!state.designerRunId && !!state.reviewerRunId
    await hydrateStreamsFromDisk()
  }

  function deleteHistory(id: string) {
    const idx = history.findIndex((h) => h.id === id)
    if (idx >= 0) history.splice(idx, 1)
    saveHistory()
  }

  // Bumped when the user asks for a fresh draft ("New"). The workspace watches
  // this to reset its editable form — lets a separate history-list component
  // trigger a form reset without owning the form state.
  const draftNonce = ref(0)
  function newDraft() {
    reset()
    draftNonce.value++
  }

  function clearHistory() {
    history.splice(0, history.length)
    saveHistory()
  }

  return {
    state,
    history,
    draftNonce,
    start,
    stop,
    continue: continueDuo,
    reset,
    newDraft,
    restore,
    deleteHistory,
    clearHistory,
    hydrateStreamsFromDisk,
  }
})

export type OrchestratorStore = ReturnType<typeof useOrchestratorStore>
