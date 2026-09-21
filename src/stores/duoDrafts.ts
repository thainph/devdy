import { defineStore } from 'pinia'
import { reactive } from 'vue'

/**
 * Unsent Duo-setup drafts, keyed by project id. The Duo workspace is mounted
 * on demand (RunView renders it with `v-if="leftTab === 'duo'"`), so switching
 * tabs — or reloading the app — unmounts {@link DuoSetupPanel} and would wipe a
 * half-composed config (goal, role instructions, engine/model picks, …).
 * Persisting the idle form here — and mirroring it to localStorage — lets the
 * config survive tab switches and full app reloads.
 *
 * Only the IDLE "new duo" form is cached. Once a duo starts it lives in the
 * orchestrator history, and a restored/finished duo drives the form through the
 * orchestrator state instead, so this draft is cleared on start / reset.
 */
export interface DuoDraft {
  sourceMode: 'new' | 'existing'
  existingDesignerId: string
  existingReviewerId: string
  designerEngine: string
  reviewerEngine: string
  designerModel: string
  reviewerModel: string
  designerLabel: string
  reviewerLabel: string
  designerInstruction: string
  reviewerInstruction: string
  permissionMode: string
  goal: string
  maxRounds: number
  consensusToken: string
  overrideBudget: boolean
}

const STORAGE_KEY = 'devdy.duoDrafts'

function str(v: unknown, fallback = ''): string {
  return typeof v === 'string' ? v : fallback
}

function sanitize(raw: unknown): DuoDraft | null {
  if (!raw || typeof raw !== 'object') return null
  const o = raw as Record<string, unknown>
  const draft: DuoDraft = {
    sourceMode: o.sourceMode === 'existing' ? 'existing' : 'new',
    existingDesignerId: str(o.existingDesignerId),
    existingReviewerId: str(o.existingReviewerId),
    designerEngine: str(o.designerEngine, 'claude'),
    reviewerEngine: str(o.reviewerEngine, 'codex'),
    designerModel: str(o.designerModel),
    reviewerModel: str(o.reviewerModel),
    designerLabel: str(o.designerLabel),
    reviewerLabel: str(o.reviewerLabel),
    designerInstruction: str(o.designerInstruction),
    reviewerInstruction: str(o.reviewerInstruction),
    permissionMode: str(o.permissionMode, 'acceptEdits'),
    goal: str(o.goal),
    maxRounds: typeof o.maxRounds === 'number' && Number.isFinite(o.maxRounds) ? o.maxRounds : 6,
    consensusToken: str(o.consensusToken, '[APPROVED]'),
    overrideBudget: o.overrideBudget === true,
  }
  return draft
}

function load(): Record<string, DuoDraft> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const obj = raw ? JSON.parse(raw) : {}
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {}
    const out: Record<string, DuoDraft> = {}
    for (const [k, v] of Object.entries(obj)) {
      const d = sanitize(v)
      if (typeof k === 'string' && d) out[k] = d
    }
    return out
  } catch {
    return {}
  }
}

export const useDuoDraftsStore = defineStore('duoDrafts', () => {
  const drafts = reactive<Record<string, DuoDraft>>(load())

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...drafts }))
    } catch {
      /* storage unavailable or quota exceeded — in-memory drafts still work */
    }
  }

  function get(key: string): DuoDraft | null {
    const d = key && drafts[key]
    return d ? { ...d } : null
  }

  function set(key: string, draft: DuoDraft) {
    if (!key) return
    drafts[key] = { ...draft }
    persist()
  }

  function clear(key: string) {
    if (!key || !(key in drafts)) return
    delete drafts[key]
    persist()
  }

  return { drafts, get, set, clear }
})
