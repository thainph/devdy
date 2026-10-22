import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@/lib/tauri'
import { linkTargetRun, type SessionLink, type SessionLinkSource } from '@/lib/sessionLinks'

/**
 * Slack threads fetched by Devdy from a thread link (Slack Web API) and
 * translated message by message into Vietnamese. They live in their own tables
 * (slack_threads / slack_messages / slack_files / slack_users). Shapes are the
 * backend contract (serde camelCase) — see
 * docs/srs-slack-thread-translate.md §10.
 */
export type SlackFetchStatus = 'pending' | 'fetching' | 'translating' | 'ready' | 'error'
export type SlackTranslateStatus = 'pending' | 'done' | 'skipped' | 'error'
export type SlackDownloadStatus = 'done' | 'pending' | 'error' | 'too_large' | 'unavailable'

export interface SlackThreadSummary {
  id: string
  title: string
  titleCustom: boolean
  projectId: string | null
  position: number
  createdAt: string
  updatedAt: string
  workspace: string | null
  workspaceDomain: string | null
  teamId: string | null
  /** Channel name WITHOUT '#'. */
  channel: string | null
  channelId: string | null
  threadUrl: string | null
  threadTs: string | null
  fetchStatus: SlackFetchStatus
  fetchErrorCode: string | null
  fetchError: string | null
  fetchedAt: string | null
  replyCount: number
  lastReplyTs: string | null
  /** ISO from lastReplyTs. */
  lastReplyAt: string | null
  /** Messages with ts > last_seen_ts; 0 when the thread was never seen. */
  newCount: number
  warnings: string[]
  legacy: boolean
  fileCount: number
  /** Parent first line (vi, plain), ≤ 140 chars. */
  preview: string
  /** Linked sessions whose run still exists. */
  linkedRunCount: number
}

export interface SlackUser {
  userId: string
  /** Never empty. */
  displayName: string
  realName: string | null
  /** Absolute path. */
  avatarPath: string | null
  isBot: boolean
}

export interface SlackReaction {
  name: string
  count: number
}

export interface SlackFile {
  id: string
  /** null = legacy zip file. */
  slackFileId: string | null
  messageTs: string | null
  name: string
  title: string | null
  mime: string | null
  filetype: string | null
  size: number
  /** Absolute; null unless downloadStatus = 'done'. */
  filePath: string | null
  downloadStatus: SlackDownloadStatus
  downloadError: string | null
  width: number | null
  height: number | null
  permalink: string | null
  /** mime image/* and downloaded. */
  isImage: boolean
}

export interface SlackMessage {
  ts: string
  seq: number
  userId: string | null
  botId: string | null
  authorName: string
  /** Absolute (user avatar or cached bot icon); null → initials. */
  avatarPath: string | null
  subtype: string | null
  /** Slack mrkdwn. */
  textRaw: string
  /** Slack mrkdwn. */
  textVi: string | null
  translateStatus: SlackTranslateStatus
  translateError: string | null
  editedTs: string | null
  /** ISO. */
  postedAt: string
  reactions: SlackReaction[]
  /** SlackFile.id. */
  fileIds: string[]
}

export interface SlackThreadView extends SlackThreadSummary {
  /** By seq. */
  messages: SlackMessage[]
  /** Every user in messages AND mentions. */
  users: Record<string, SlackUser>
  /** channel_id → name. */
  channels: Record<string, string>
  /** Slack files + legacy files (slackFileId null). */
  files: SlackFile[]
  /** Only while legacy. */
  legacyContent: string | null
}

export type SlackProgressPhase =
  | 'queued'
  | 'auth'
  | 'messages'
  | 'users'
  | 'files'
  | 'translate'
  | 'done'

export interface SlackThreadProgress {
  id: string
  status: SlackFetchStatus
  phase: SlackProgressPhase
  done: number
  total: number | null
  errorCode: string | null
  error: string | null
}

export interface SlackThreadsChanged {
  id: string
  action: 'created' | 'updated' | 'deleted'
}

export interface AddSlackThreadResult {
  id: string
  status: 'created' | 'existing'
  fetchStatus: SlackFetchStatus
}

/** A session (run) linked to a thread. */
export interface SlackThreadRunLink extends SessionLink {
  threadId: string
}

/** Threads linked to a session, most recently linked first. */
export function listRunSlackThreads(runId: string): Promise<SlackThreadSummary[]> {
  return invoke<SlackThreadSummary[]>('list_run_slack_threads', { runId })
}

export interface SlackTokenStatus {
  state: 'ok' | 'none' | 'multiple'
  sources: { serverId: string; serverName: string }[]
}

/** Data changes: create / update / delete / status / messages / mark seen. */
export const SLACK_THREADS_CHANGED = 'slack-threads://changed'
/** Job progress (phase + done/total), throttled by the backend. */
export const SLACK_THREADS_PROGRESS = 'slack-threads://progress'

/** A job is queued or running for the thread. */
export function isBusy(status: SlackFetchStatus): boolean {
  return status === 'pending' || status === 'fetching' || status === 'translating'
}

/**
 * Command errors are `"<code>: <detail>"`; returns the code part, or null when
 * the message does not look like one (plain engine / IPC error).
 */
export function errorCode(e: unknown): { code: string | null; detail: string } {
  const msg = String(e instanceof Error ? e.message : e)
  const m = /^([a-z_]+):\s*(.*)$/s.exec(msg)
  return m ? { code: m[1], detail: m[2] } : { code: null, detail: msg }
}

export const useSlackThreadsStore = defineStore('slackThreads', () => {
  const threads = ref<SlackThreadSummary[]>([])
  const selectedId = ref<string | null>(null)
  /** The open thread (full view), or null. */
  const view = ref<SlackThreadView | null>(null)
  /** Sessions linked to the open thread. */
  const links = ref<SlackThreadRunLink[]>([])
  const progress = ref<Record<string, SlackThreadProgress>>({})
  const loading = ref(false)
  /** Last list load failure (cleared by the next successful load). */
  const error = ref<string | null>(null)
  /** Last failure loading the open thread (cleared on success / selection change). */
  const viewError = ref<string | null>(null)
  /** The search query the list was last fetched with. */
  const query = ref('')

  // Drops stale responses when two fetches overlap (typing in the search box).
  let listSeq = 0
  let viewSeq = 0

  async function fetchList(q?: string | null) {
    if (q !== undefined) query.value = q ?? ''
    const seq = ++listSeq
    loading.value = true
    try {
      const rows = await invoke<SlackThreadSummary[]>('list_slack_threads', {
        projectId: null,
        query: query.value.trim() || null,
      })
      if (seq !== listSeq) return
      threads.value = rows
      error.value = null
    } catch (e) {
      if (seq === listSeq) error.value = String(e)
    } finally {
      if (seq === listSeq) loading.value = false
    }
  }

  /** Select a thread and load its full view (null clears the pane). */
  async function open(id: string | null) {
    selectedId.value = id
    viewError.value = null
    if (!id) {
      view.value = null
      links.value = []
      return
    }
    if (view.value?.id !== id) {
      view.value = null
      links.value = []
    }
    await reloadView()
  }

  /** Refetch the open thread; a missing row clears the selection. */
  async function reloadView() {
    const id = selectedId.value
    if (!id) return
    const seq = ++viewSeq
    try {
      const [v, l] = await Promise.all([
        invoke<SlackThreadView>('get_slack_thread', { id }),
        // Links are an extra: a failure leaves the chips as they were.
        invoke<SlackThreadRunLink[]>('list_slack_thread_runs', { threadId: id }).catch(() => null),
      ])
      if (seq === viewSeq && selectedId.value === id) {
        view.value = v
        if (l) links.value = l
        viewError.value = null
      }
    } catch (e) {
      if (seq !== viewSeq || selectedId.value !== id) return
      if (errorCode(e).code === 'not_found') {
        view.value = null
        links.value = []
        selectedId.value = null
      } else {
        viewError.value = String(e)
      }
    }
  }

  function patchSummary(row: SlackThreadSummary) {
    const i = threads.value.findIndex((x) => x.id === row.id)
    if (i !== -1) threads.value[i] = row
    if (view.value?.id === row.id) view.value = { ...view.value, ...row }
  }

  async function add(url: string, projectId: string | null): Promise<AddSlackThreadResult> {
    const res = await invoke<AddSlackThreadResult>('add_slack_thread', { url, projectId })
    await fetchList()
    return res
  }

  async function refresh(id: string): Promise<SlackThreadSummary> {
    const row = await invoke<SlackThreadSummary>('refresh_slack_thread', { id })
    patchSummary(row)
    return row
  }

  async function retranslate(id: string): Promise<SlackThreadSummary> {
    const row = await invoke<SlackThreadSummary>('retranslate_slack_thread', { id })
    patchSummary(row)
    return row
  }

  /** Vietnamese reply draft → English (popover translate sidecar). */
  async function translateReply(text: string): Promise<string> {
    return invoke<string>('translate_slack_reply', { text })
  }

  /** Post a reply to the thread as the token's user; the thread then refreshes. */
  async function postReply(id: string, text: string): Promise<SlackThreadSummary> {
    const row = await invoke<SlackThreadSummary>('post_slack_thread_reply', { id, text })
    patchSummary(row)
    return row
  }

  async function markSeen(id: string) {
    await invoke('mark_slack_thread_seen', { id })
    const row = threads.value.find((x) => x.id === id)
    if (row) row.newCount = 0
  }

  /** Optimistic; the backend trims and rejects empty / > 200 chars (rolled back). */
  async function rename(id: string, title: string): Promise<SlackThreadSummary> {
    const row = threads.value.find((x) => x.id === id)
    const prev = row ? { title: row.title, titleCustom: row.titleCustom } : null
    if (row) Object.assign(row, { title: title.trim(), titleCustom: true })
    try {
      const updated = await invoke<SlackThreadSummary>('rename_slack_thread', { id, title })
      patchSummary(updated)
      return updated
    } catch (e) {
      if (row && prev) Object.assign(row, prev)
      throw e
    }
  }

  async function setProject(id: string, projectId: string | null) {
    const row = threads.value.find((x) => x.id === id)
    const prev = row ? row.projectId : null
    if (row) row.projectId = projectId
    if (view.value?.id === id) view.value.projectId = projectId
    try {
      await invoke('set_slack_thread_project', { id, projectId })
    } catch (e) {
      if (row) row.projectId = prev
      if (view.value?.id === id) view.value.projectId = prev
      throw e
    }
  }

  /** Persist a new order (ids top first). */
  async function reorder(ids: string[]) {
    const snapshot = threads.value.slice()
    const rank = new Map(ids.map((id, i) => [id, i]))
    threads.value = threads.value
      .slice()
      .sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0))
    try {
      await invoke('reorder_slack_threads', { ids })
    } catch (e) {
      threads.value = snapshot
      throw e
    }
  }

  /**
   * Delete threads (rows, messages, files on disk), optimistically. The caller
   * confirms first (useConfirm lives in the window that owns the dialog host).
   */
  async function remove(ids: string[]) {
    if (ids.length === 0) return
    const target = new Set(ids)
    const snapshot = threads.value.slice()
    threads.value = threads.value.filter((x) => !target.has(x.id))
    if (selectedId.value && target.has(selectedId.value)) await open(null)
    try {
      await invoke('delete_slack_threads', { ids })
    } catch (e) {
      threads.value = snapshot
      throw e
    }
  }

  /** Link the thread to a session, then reload the open thread / list. */
  async function linkRun(threadId: string, runId: string, source: SessionLinkSource) {
    const res = await linkTargetRun({ kind: 'slack', id: threadId }, runId, source)
    if (view.value?.id === threadId) await reloadView()
    return res
  }

  /** Optimistic; restored on failure. */
  async function unlinkRun(threadId: string, runId: string) {
    const snapshot = links.value.slice()
    if (view.value?.id === threadId) links.value = links.value.filter((l) => l.runId !== runId)
    const row = threads.value.find((x) => x.id === threadId)
    if (row) row.linkedRunCount = Math.max(0, row.linkedRunCount - 1)
    try {
      await invoke('unlink_slack_thread_run', { threadId, runId })
    } catch (e) {
      if (view.value?.id === threadId) links.value = snapshot
      if (row) row.linkedRunCount += 1
      throw e
    }
  }

  async function tokenStatus(): Promise<SlackTokenStatus> {
    return invoke<SlackTokenStatus>('get_slack_token_status')
  }

  function applyProgress(p: SlackThreadProgress) {
    if (!p?.id) return
    progress.value = { ...progress.value, [p.id]: p }
    const row = threads.value.find((x) => x.id === p.id)
    if (row && row.fetchStatus !== p.status) row.fetchStatus = p.status
    if (view.value?.id === p.id && view.value.fetchStatus !== p.status) {
      view.value.fetchStatus = p.status
    }
  }

  function onDeleted(id: string) {
    threads.value = threads.value.filter((x) => x.id !== id)
    const next = { ...progress.value }
    delete next[id]
    progress.value = next
    if (selectedId.value === id) {
      selectedId.value = null
      view.value = null
      links.value = []
    }
  }

  return {
    threads,
    selectedId,
    view,
    links,
    progress,
    loading,
    error,
    viewError,
    query,
    fetchList,
    open,
    reloadView,
    add,
    refresh,
    retranslate,
    translateReply,
    postReply,
    markSeen,
    rename,
    setProject,
    reorder,
    remove,
    linkRun,
    unlinkRun,
    tokenStatus,
    applyProgress,
    onDeleted,
  }
})
