/**
 * PR tab state for the Controller ({@link FEATURE_PR}): the Host's PR inbox
 * (the desktop PR Inbox) and the reviews this phone starts from it.
 *
 * The list arrives as `pr_inbox` frames: once per `pr_inbox_list` /
 * `pr_inbox_refresh`, then after every fetch on the Host (its poller, the
 * desktop, this phone) and every review linked to a PR — so a started review
 * shows up without a re-fetch. Review actions resolve to the run they started
 * or resumed, so the screen can open it.
 */
import { reactive } from 'vue'
import type { CommandAckHandler } from './connection'
import {
  prInboxListCmd,
  prInboxRefreshCmd,
  prReviewAgainCmd,
  prReviewStartCmd,
  type CmdPayload,
  type PrInboxInfo,
  type StreamPayload,
} from './protocol.ts'

/** `ControllerConnection.sendCommand`; false when the room is not live. */
export type SendCommand = (cmd: CmdPayload, onAck?: CommandAckHandler) => boolean

export interface PrState {
  /** null until the first `pr_inbox` arrives. */
  items: PrInboxInfo[] | null
  /** When the Host last fetched from GitHub; null before its first fetch. */
  fetchedAt: string | null
  /** A fetch is running on the Host. */
  refreshing: boolean
  /** Accounts whose last fetch failed. */
  failedAccounts: string[]
  /** The last fetch failed (code). */
  fetchError: string | null
  /** The list request was refused / never answered. */
  loadFailed: boolean
  /** PR keys with a start / re-review in flight from this phone. */
  busy: Record<string, true>
  /** The last refused action, already human-readable. */
  error: string | null
}

export function createPrStore(send: SendCommand, describeError: (reason: string | null) => string) {
  const state = reactive<PrState>({
    items: null,
    fetchedAt: null,
    refreshing: false,
    failedAccounts: [],
    fetchError: null,
    loadFailed: false,
    busy: {},
    error: null,
  })

  function apply(p: StreamPayload): void {
    if (p.kind !== 'pr_inbox') return
    state.items = p.items
    state.fetchedAt = p.fetched_at
    state.refreshing = p.refreshing
    state.failedAccounts = p.failed_accounts
    state.fetchError = p.error
    state.loadFailed = false
  }

  function read(cmd: CmdPayload): void {
    state.loadFailed = false
    const sent = send(cmd, (accepted, reason) => {
      if (accepted) return
      state.refreshing = false
      if (state.items === null) state.loadFailed = true
      state.error = describeError(reason === 'host_ack_timeout' ? 'load_timeout' : (reason ?? null))
    })
    if (!sent) {
      if (state.items === null) state.loadFailed = true
      state.error = describeError('not_connected')
    }
  }

  /** The cached list (the Host re-fetches it behind when it is stale). */
  function load(): void {
    read(prInboxListCmd())
  }

  /** Fetch from GitHub now. */
  function refresh(): void {
    state.refreshing = true
    read(prInboxRefreshCmd())
  }

  /** Send a review action; resolves to its run id, or null when refused. */
  function act(key: string, cmd: CmdPayload): Promise<string | null> {
    if (state.busy[key]) return Promise.resolve(null)
    state.error = null
    return new Promise((resolve) => {
      const sent = send(cmd, (accepted, reason, runId) => {
        delete state.busy[key]
        if (!accepted) state.error = describeError(reason ?? null)
        resolve(accepted ? (runId ?? null) : null)
      })
      if (sent) {
        state.busy[key] = true
      } else {
        state.error = describeError('not_connected')
        resolve(null)
      }
    })
  }

  /** Review the PR in a new session (or get the one it already has). */
  function review(key: string): Promise<string | null> {
    return act(key, prReviewStartCmd(key))
  }

  /** Continue the PR's finished review with the re-review prompt. */
  function reviewAgain(key: string): Promise<string | null> {
    return act(key, prReviewAgainCmd(key))
  }

  /** After a reconnect: re-fetch if the tab has been opened. */
  function reload(): void {
    if (state.items !== null || state.loadFailed) load()
  }

  function clearError(): void {
    state.error = null
  }

  function reset(): void {
    state.items = null
    state.fetchedAt = null
    state.refreshing = false
    state.failedAccounts = []
    state.fetchError = null
    state.loadFailed = false
    state.busy = {}
    state.error = null
  }

  return { state, apply, load, refresh, review, reviewAgain, reload, clearError, reset }
}

export type PrStore = ReturnType<typeof createPrStore>
