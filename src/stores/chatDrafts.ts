import { defineStore } from 'pinia'
import { reactive } from 'vue'

/**
 * Unsent chatbox drafts, keyed by project id. RunView is remounted whenever the
 * active project changes (its RouterView key is `run-<projectId>`), which would
 * otherwise wipe whatever the user was composing. Persisting the draft here —
 * and mirroring it to localStorage — lets the composer survive both project
 * switches and full app reloads. We keep the typed text, pasted images
 * (base64) and attached files (by path).
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

export const useChatDraftsStore = defineStore('chatDrafts', () => {
  const drafts = reactive<Record<string, ChatDraft>>(load())

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...drafts }))
    } catch {
      /* storage unavailable or quota exceeded — in-memory drafts still work */
    }
  }

  function get(projectId: string): ChatDraft {
    const d = projectId && drafts[projectId]
    return d ? { text: d.text, images: [...d.images], files: [...d.files] } : emptyDraft()
  }

  /** Save (or clear, when empty) the draft for a project. */
  function set(projectId: string, draft: ChatDraft) {
    if (!projectId) return
    if (isEmpty(draft)) {
      if (!(projectId in drafts)) return
      delete drafts[projectId]
    } else {
      drafts[projectId] = { text: draft.text, images: [...draft.images], files: [...draft.files] }
    }
    persist()
  }

  function clear(projectId: string) {
    set(projectId, emptyDraft())
  }

  return { drafts, get, set, clear }
})
