import { defineStore } from 'pinia'

/**
 * Where the reader was parked in a session, so reopening it (switching back to
 * its project tab, picking it from History, a reload) lands on the same spot
 * instead of the end of the log.
 *
 * A pixel `scrollTop` cannot be replayed: the history pane only loads a window
 * of the log, and entry heights change with re-wrapping and async rendering. So
 * the position is anchored on the prompt (user turn) at the top of the viewport
 * — identified by its index in the whole session and the byte offset of its
 * record in the log — plus how far the viewport had scrolled past it.
 *
 * Only sessions the reader left mid-way get a memo; leaving at the bottom
 * forgets it, which restores the default "open at the latest output".
 */
export interface SessionScrollMemo {
  projectId: string
  /** Index of the anchor prompt in the whole session; -1 = pixel fallback. */
  turnSeq: number
  /** Byte offset of the anchor prompt's record in the log, when known. */
  turnOffset?: number
  /** One-line label of the anchor prompt, to verify it is still the same turn. */
  turnLabel: string
  /** px between the anchor prompt's top and the viewport top. */
  delta: number
  /** Raw scrollTop — only used when there is no prompt to anchor on. */
  scrollTop: number
  updatedAt: number
}

const STORAGE_KEY = 'devdy.sessionScroll'
/** Most recent sessions remembered per project. */
const MAX_PER_PROJECT = 10
const MAX_TOTAL = 300

function sanitize(raw: unknown): SessionScrollMemo | null {
  if (!raw || typeof raw !== 'object') return null
  const o = raw as Record<string, unknown>
  if (typeof o.projectId !== 'string' || typeof o.turnSeq !== 'number') return null
  return {
    projectId: o.projectId,
    turnSeq: o.turnSeq,
    turnOffset: typeof o.turnOffset === 'number' ? o.turnOffset : undefined,
    turnLabel: typeof o.turnLabel === 'string' ? o.turnLabel : '',
    delta: typeof o.delta === 'number' ? o.delta : 0,
    scrollTop: typeof o.scrollTop === 'number' ? o.scrollTop : 0,
    updatedAt: typeof o.updatedAt === 'number' ? o.updatedAt : 0,
  }
}

// Read straight from storage on every call rather than caching in memory:
// detached session windows run their own RunView (and their own Pinia), so an
// in-memory copy would overwrite memos another window saved meanwhile.
function load(): Record<string, SessionScrollMemo> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const obj = raw ? JSON.parse(raw) : {}
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {}
    const out: Record<string, SessionScrollMemo> = {}
    for (const [k, v] of Object.entries(obj)) {
      const m = sanitize(v)
      if (m) out[k] = m
    }
    return out
  } catch {
    return {}
  }
}

function persist(memos: Record<string, SessionScrollMemo>) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(memos))
  } catch {
    /* storage unavailable or quota exceeded — the position just isn't kept */
  }
}

// Keep the newest MAX_PER_PROJECT memos of each project, and MAX_TOTAL overall.
function prune(memos: Record<string, SessionScrollMemo>) {
  const byNewest = Object.entries(memos).sort((a, b) => b[1].updatedAt - a[1].updatedAt)
  const perProject = new Map<string, number>()
  byNewest.forEach(([runId, m], i) => {
    const n = (perProject.get(m.projectId) ?? 0) + 1
    perProject.set(m.projectId, n)
    if (n > MAX_PER_PROJECT || i >= MAX_TOTAL) delete memos[runId]
  })
}

export const useSessionScrollStore = defineStore('sessionScroll', () => {
  function get(runId: string): SessionScrollMemo | null {
    return (runId && load()[runId]) || null
  }

  function save(runId: string, memo: Omit<SessionScrollMemo, 'updatedAt'>) {
    if (!runId) return
    const memos = load()
    memos[runId] = { ...memo, updatedAt: Date.now() }
    prune(memos)
    persist(memos)
  }

  function forget(runId: string) {
    const memos = load()
    if (!(runId in memos)) return
    delete memos[runId]
    persist(memos)
  }

  return { get, save, forget }
})
