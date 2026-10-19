// Remember and restore the reading position of a session.
//
// The anchor is the prompt (user turn) at the top of the viewport — see
// `SessionScrollMemo` for why a raw scrollTop can't be replayed. Capturing is
// debounced off the scroll handler and flushed right before the view switches
// to another run or unmounts (after that, the DOM no longer shows this run).
import { nextTick, type Ref } from 'vue'
import { useSessionScrollStore, type SessionScrollMemo } from '@/stores/sessionScroll'
import type { PromptTurn } from '@/composables/useTurnNavigator'

const CAPTURE_DEBOUNCE_MS = 300
/** How long late layout (markdown, mermaid, images) may still shift the anchor. */
const HOLD_MS = 1500

interface Options {
  runId: Ref<string | null>
  projectId: Ref<string>
  scrollEl: Ref<HTMLElement | null>
  /** Prompts in the loaded window, in order. */
  turns: Ref<PromptTurn[]>
  /** Session-wide index of the loaded window's first prompt. */
  loadedTurnOffset: Ref<number>
  /** Whole-session prompt list (offsets known once the log scan resolves). */
  navTurns: Ref<{ label: string; offset?: number }[]>
  activeEntryIndex: () => number
  isNearBottom: (el: HTMLElement) => boolean
  /** Whether the pane shows real content (not a loading / empty state). */
  isReady: () => boolean
  /** Load a window starting at the prompt `seq` (byte `offset`), if the pane supports it. */
  jumpTo: (seq: number, offset: number) => Promise<void>
}

function frame() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
}

export function useScrollMemory(o: Options) {
  const store = useSessionScrollStore()

  let pendingTimer: ReturnType<typeof setTimeout> | null = null
  let pendingRunId: string | null = null
  let restoring = false
  let releaseHold: (() => void) | null = null

  function turnEl(root: HTMLElement, entryIndex: number) {
    return root.querySelector<HTMLElement>(`[data-user-turn="${entryIndex}"]`)
  }

  function capture(runId: string) {
    const el = o.scrollEl.value
    if (!el || restoring || runId !== o.runId.value || !o.isReady()) return
    if (o.isNearBottom(el)) {
      store.forget(runId)
      return
    }
    const turns = o.turns.value
    // Above the first loaded prompt → anchor on it with a negative delta.
    const local = Math.max(0, turns.findIndex((t) => t.entryIndex === o.activeEntryIndex()))
    const turn = turns[local]
    const node = turn ? turnEl(el, turn.entryIndex) : null
    if (!turn || !node) {
      store.save(runId, { projectId: o.projectId.value, turnSeq: -1, turnLabel: '', delta: 0, scrollTop: el.scrollTop })
      return
    }
    const seq = o.loadedTurnOffset.value + local
    store.save(runId, {
      projectId: o.projectId.value,
      turnSeq: seq,
      turnOffset: o.navTurns.value[seq]?.offset,
      turnLabel: turn.label,
      delta: el.getBoundingClientRect().top - node.getBoundingClientRect().top,
      scrollTop: el.scrollTop,
    })
  }

  /** Call from the pane's scroll handler. */
  function schedule() {
    const runId = o.runId.value
    if (!runId || restoring) return
    if (pendingTimer && pendingRunId !== runId) flush()
    if (pendingTimer) clearTimeout(pendingTimer)
    pendingRunId = runId
    pendingTimer = setTimeout(flush, CAPTURE_DEBOUNCE_MS)
  }

  /** Capture a pending position now — call before the pane switches runs or unmounts. */
  function flush() {
    if (!pendingTimer) return
    clearTimeout(pendingTimer)
    pendingTimer = null
    const runId = pendingRunId
    pendingRunId = null
    if (runId) capture(runId)
  }

  // The loaded prompt matching the memo: by session index first, then by label
  // (the index can drift if the whole-session scan hadn't resolved at capture).
  function locate(memo: SessionScrollMemo): number | null {
    const turns = o.turns.value
    const byIndex = turns[memo.turnSeq - o.loadedTurnOffset.value]
    if (byIndex && byIndex.label === memo.turnLabel) return byIndex.entryIndex
    for (let i = turns.length - 1; i >= 0; i--) {
      if (turns[i].label === memo.turnLabel) return turns[i].entryIndex
    }
    return null
  }

  function apply(entryIndex: number, delta: number) {
    const root = o.scrollEl.value
    const node = root && turnEl(root, entryIndex)
    if (!root || !node) return
    const current = root.getBoundingClientRect().top - node.getBoundingClientRect().top
    if (current !== delta) root.scrollTop += delta - current
  }

  // Markdown, mermaid and images keep laying out after the first frame, which
  // shifts the anchor. Re-apply on every resize for a short while, until the
  // reader takes over.
  function hold(entryIndex: number, delta: number) {
    releaseHold?.()
    const root = o.scrollEl.value
    if (!root) return
    const ro = new ResizeObserver(() => apply(entryIndex, delta))
    ro.observe(root)
    for (const child of Array.from(root.children)) ro.observe(child)
    const events = ['wheel', 'pointerdown', 'keydown', 'touchstart'] as const
    const timer = setTimeout(() => release(), HOLD_MS)
    function release() {
      ro.disconnect()
      clearTimeout(timer)
      for (const e of events) root!.removeEventListener(e, release)
      if (releaseHold === release) releaseHold = null
    }
    for (const e of events) root.addEventListener(e, release, { passive: true })
    releaseHold = release
  }

  function peek(runId: string | null) {
    return runId ? store.get(runId) : null
  }

  /**
   * Put the pane back where the reader left `runId`. Resolves false when there
   * is nothing to restore (or the anchor is gone) — the caller then falls back
   * to its default of showing the latest output.
   */
  async function restore(runId: string, logSize?: number): Promise<boolean> {
    const memo = store.get(runId)
    if (!memo) return false
    restoring = true
    try {
      await nextTick()
      await frame()
      if (o.runId.value !== runId || !o.scrollEl.value) return false

      if (memo.turnSeq < 0) {
        if (o.turns.value.length) return false
        o.scrollEl.value.scrollTop = memo.scrollTop
        return true
      }

      let entryIndex = locate(memo)
      const offset = memo.turnOffset
      if (entryIndex === null && offset !== undefined && (logSize === undefined || offset < logSize)) {
        await o.jumpTo(memo.turnSeq, offset)
        await nextTick()
        await frame()
        if (o.runId.value !== runId) return false
        entryIndex = locate(memo)
      }
      if (entryIndex === null) {
        store.forget(runId)
        return false
      }
      apply(entryIndex, memo.delta)
      hold(entryIndex, memo.delta)
      return true
    } finally {
      restoring = false
    }
  }

  function forget(runId: string) {
    if (pendingRunId === runId) {
      if (pendingTimer) clearTimeout(pendingTimer)
      pendingTimer = null
      pendingRunId = null
    }
    releaseHold?.()
    store.forget(runId)
  }

  function dispose() {
    flush()
    releaseHold?.()
  }

  return { schedule, flush, peek, restore, forget, dispose }
}
