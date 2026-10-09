import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

/**
 * Captures pushed in by the Chrome extension over the local Inbox API (or
 * imported by hand from a .zip / .md): Slack threads (`kind: 'slack'`) and web
 * pages (`kind: 'web'`), persisted in the app SQLite DB (captures +
 * capture_attachments) via Tauri commands. Mirrors the notes store: array order
 * follows the `position` column (per kind), mutations apply optimistically and
 * roll back on error.
 *
 * One list holds every kind; views filter with `ofKind()`. Shapes are the
 * backend contract (serde camelCase) — see docs/srs-web-page-capture.md §4.
 */
export type CaptureKind = 'slack' | 'web'

export const CAPTURE_KINDS: CaptureKind[] = ['slack', 'web']

export interface CaptureAttachment {
  id: string
  captureId: string
  name: string
  /** Absolute path on disk. */
  filePath: string
  size: number
  mime: string | null
  createdAt: string
}

export interface Capture {
  id: string
  kind: CaptureKind
  title: string
  content: string
  /** True once the user renamed it: re-exports of the same source keep that title. */
  titleCustom: boolean
  projectId: string | null
  position: number
  createdAt: string
  updatedAt: string
  exportedAt: string | null
  attachmentCount: number
  // Slack
  workspace: string | null
  channel: string | null
  threadUrl: string | null
  threadTs: string | null
  // Web
  sourceUrl: string | null
  siteName: string | null
  author: string | null
  publishedAt: string | null
  description: string | null
  /** Web: only the user's selection was captured, not the whole page. */
  selection: boolean
}

export interface CaptureDetail extends Capture {
  attachments: CaptureAttachment[]
}

export interface InboxApiInfo {
  running: boolean
  port: number | null
  token: string
  baseUrl: string | null
}

export interface CaptureImportResult {
  id: string
  status: 'created' | 'updated' | string
}

/** Emitted by the backend on every create / update / delete. */
export const CAPTURES_CHANGED = 'captures://changed'

export interface CapturesChanged {
  id: string
  kind: CaptureKind
  action: 'created' | 'updated' | 'deleted'
}

/** `#channel` with exactly one leading `#`, or '' when the export had none. */
export function channelLabel(c: Pick<Capture, 'channel'>): string {
  if (!c.channel) return ''
  return c.channel.startsWith('#') ? c.channel : `#${c.channel}`
}

/** A web capture's host without `www.`, or '' when the URL is missing / invalid. */
export function captureDomain(c: Pick<Capture, 'sourceUrl'>): string {
  if (!c.sourceUrl) return ''
  try {
    return new URL(c.sourceUrl).hostname.replace(/^www\./, '')
  } catch {
    return ''
  }
}

/** Where the capture points back to: the Slack permalink or the page URL. */
export function captureOriginalUrl(c: Capture): string | null {
  return c.kind === 'slack' ? c.threadUrl : c.sourceUrl
}

export const useCapturesStore = defineStore('captures', () => {
  const captures = ref<Capture[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  /** All kinds, in `position` order (filter with `ofKind`). */
  async function fetchCaptures() {
    loading.value = true
    error.value = null
    try {
      captures.value = await invoke<Capture[]>('list_captures', { kind: null, projectId: null })
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  function ofKind(kind: CaptureKind): Capture[] {
    return captures.value.filter((c) => c.kind === kind)
  }

  function replace(updated: Capture) {
    const i = captures.value.findIndex((x) => x.id === updated.id)
    if (i !== -1) captures.value[i] = updated
  }

  /** Full capture incl. attachments (not cached: the detail window owns it). */
  async function getDetail(id: string): Promise<CaptureDetail> {
    return invoke<CaptureDetail>('get_capture', { id })
  }

  /** Rewrite the body (and/or title). Returns the stored row. */
  async function update(id: string, patch: { title?: string; content?: string }) {
    const updated = await invoke<Capture>('update_capture', {
      id,
      title: patch.title,
      content: patch.content,
    })
    replace(updated)
    return updated
  }

  /**
   * Give the capture a custom title (kept across re-exports). Optimistic; the
   * backend trims and rejects empty / > 200 chars, which rolls back and rethrows.
   */
  async function rename(id: string, title: string): Promise<Capture> {
    const row = captures.value.find((x) => x.id === id)
    const prev = row ? { title: row.title, titleCustom: row.titleCustom } : null
    if (row) {
      row.title = title.trim() // optimistic
      row.titleCustom = true
    }
    try {
      const updated = await invoke<Capture>('rename_capture', { id, title })
      replace(updated)
      return updated
    } catch (e) {
      if (row && prev) Object.assign(row, prev)
      throw e
    }
  }

  async function setProject(id: string, projectId: string | null) {
    const row = captures.value.find((x) => x.id === id)
    const prev = row ? row.projectId : null
    if (row) row.projectId = projectId // optimistic
    try {
      await invoke('set_capture_project', { id, projectId })
    } catch (e) {
      error.value = String(e)
      if (row) row.projectId = prev
      throw e
    }
  }

  /** Delete captures (rows + attachment files on disk), optimistically. */
  async function removeMany(ids: string[]) {
    if (ids.length === 0) return
    const target = new Set(ids)
    const snapshot = captures.value.slice()
    captures.value = captures.value.filter((x) => !target.has(x.id)) // optimistic
    try {
      await invoke('delete_captures', { ids })
    } catch (e) {
      error.value = String(e)
      captures.value = snapshot
      throw e
    }
  }

  /** Persist a new order for one kind's captures (ids of that kind, top first). */
  async function reorder(kind: CaptureKind, ids: string[]) {
    const snapshot = captures.value.slice()
    const rank = new Map(ids.map((id, i) => [id, i]))
    const others = captures.value.filter((c) => c.kind !== kind)
    const mine = captures.value
      .filter((c) => c.kind === kind)
      .sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0))
    captures.value = [...mine, ...others]
    try {
      await invoke('reorder_captures', { ids })
    } catch (e) {
      error.value = String(e)
      captures.value = snapshot
    }
  }

  /** Manual fallback for the extension: import a .zip / .md picked from disk. */
  async function importFile(path: string, kind: CaptureKind): Promise<CaptureImportResult> {
    const res = await invoke<CaptureImportResult>('import_capture_file', { path, kind })
    await fetchCaptures()
    return res
  }

  async function getInboxApiInfo(): Promise<InboxApiInfo> {
    return invoke<InboxApiInfo>('get_inbox_api_info')
  }

  async function regenerateInboxApiToken(): Promise<InboxApiInfo> {
    return invoke<InboxApiInfo>('regenerate_inbox_api_token')
  }

  return {
    captures,
    loading,
    error,
    fetchCaptures,
    ofKind,
    getDetail,
    update,
    rename,
    setProject,
    removeMany,
    reorder,
    importFile,
    getInboxApiInfo,
    regenerateInboxApiToken,
  }
})
