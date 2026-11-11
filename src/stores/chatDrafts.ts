import { defineStore } from 'pinia'

/**
 * Unsent chatbox drafts, keyed by SESSION (run id). Switching between sessions,
 * leaving RunView and coming back, or a full app reload would otherwise wipe
 * whatever the user was composing. Keying by run id means each session keeps its
 * own draft instead of sharing one project-wide slot that the next session would
 * clobber. Persisting here — and mirroring to localStorage — lets the composer
 * survive session switches and reloads. We keep the typed text, pasted images
 * (base64) and attached files (by path). The key is an opaque string, so any
 * stable id works.
 */
export interface DraftImage {
  media_type: string
  data: string // raw base64 (no data: prefix)
}

export interface DraftFile {
  name: string
  path: string
}

export interface ChatDraft {
  text: string
  images: DraftImage[]
  files: DraftFile[]
}

const STORAGE_KEY = 'devdy.chatDrafts'

function emptyDraft(): ChatDraft {
  return { text: '', images: [], files: [] }
}

function isEmpty(d: ChatDraft): boolean {
  return !d.text && d.images.length === 0 && d.files.length === 0
}

function sanitize(raw: unknown): ChatDraft | null {
  if (!raw || typeof raw !== 'object') return null
  const obj = raw as Record<string, unknown>
  const text = typeof obj.text === 'string' ? obj.text : ''
  const images = Array.isArray(obj.images)
    ? obj.images
        .filter(
          (i): i is DraftImage =>
            !!i && typeof (i as DraftImage).media_type === 'string' && typeof (i as DraftImage).data === 'string',
        )
        .map((i) => ({ media_type: i.media_type, data: i.data }))
    : []
  const files = Array.isArray(obj.files)
    ? obj.files
        .filter(
          (f): f is DraftFile =>
            !!f && typeof (f as DraftFile).name === 'string' && typeof (f as DraftFile).path === 'string',
        )
        .map((f) => ({ name: f.name, path: f.path }))
    : []
  const draft: ChatDraft = { text, images, files }
  return isEmpty(draft) ? null : draft
}

function load(): Record<string, ChatDraft> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const obj = raw ? JSON.parse(raw) : {}
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {}
    const out: Record<string, ChatDraft> = {}
    for (const [k, v] of Object.entries(obj)) {
      const d = sanitize(v)
      if (typeof k === 'string' && d) out[k] = d
    }
    return out
  } catch {
    return {}
  }
}

// Writing localStorage is synchronous and serializes EVERY session's draft
// (pasted images included, as base64), so it must not run per keystroke. The
// in-memory map updates immediately; the disk mirror is debounced and flushed on
// window teardown and on session switch (RunView calls `flush`).
const PERSIST_DEBOUNCE_MS = 400

export const useChatDraftsStore = defineStore('chatDrafts', () => {
  // Plain, non-reactive map: drafts are only ever read through `get()` (a copy),
  // so making them reactive would just deep-proxy large base64 payloads.
  const drafts: Record<string, ChatDraft> = load()
  let persistTimer: ReturnType<typeof setTimeout> | null = null

  function write(value: Record<string, ChatDraft>): boolean {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(value))
      return true
    } catch {
      return false
    }
  }

  function persistNow() {
    if (persistTimer) { clearTimeout(persistTimer); persistTimer = null }
    if (write(drafts)) return
    // Most likely the quota: pasted images are the bulk of the payload, so keep
    // at least text + file paths across a reload. In-memory drafts (and thus
    // session switches) still keep the images.
    const lite: Record<string, ChatDraft> = {}
    for (const [k, d] of Object.entries(drafts)) {
      const slim = { text: d.text, images: [], files: d.files }
      if (!isEmpty(slim)) lite[k] = slim
    }
    write(lite)
  }

  function schedulePersist() {
    if (persistTimer) clearTimeout(persistTimer)
    persistTimer = setTimeout(persistNow, PERSIST_DEBOUNCE_MS)
  }

  /** Write any pending change to localStorage right away. */
  function flush() {
    if (persistTimer) persistNow()
  }

  if (typeof window !== 'undefined') {
    window.addEventListener('beforeunload', flush)
    window.addEventListener('pagehide', flush)
  }

  function get(key: string): ChatDraft {
    const d = key && drafts[key]
    return d ? { text: d.text, images: [...d.images], files: [...d.files] } : emptyDraft()
  }

  /** Save (or clear, when empty) the draft for a session/key. */
  function set(key: string, draft: ChatDraft) {
    if (!key) return
    if (isEmpty(draft)) {
      if (!(key in drafts)) return
      delete drafts[key]
    } else {
      drafts[key] = { text: draft.text, images: [...draft.images], files: [...draft.files] }
    }
    schedulePersist()
  }

  function clear(key: string) {
    set(key, emptyDraft())
  }

  return { get, set, clear, flush }
})
