import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'
import { PR_GROUP_BY_OPTIONS, type PrGroupBy } from '@/lib/prInboxGrouping'
import { normalizeLogin, sanitizeBotLogins, sanitizeFilters, emptyFilters, type PrFilters } from '@/lib/prInboxFilters'

/** A PR awaiting the user's review — mirrors the Rust `PrInboxItem`. */
export interface PrInboxItem {
  account_id: string
  account_label: string
  owner: string
  repo: string
  number: number
  title: string
  html_url: string
  author_login: string
  author_avatar_url: string | null
  /** GitHub reports the author as a Bot, or the login ends with `[bot]`. */
  author_is_bot: boolean
  draft: boolean
  labels: string[]
  updated_at: string
  mapped: boolean
  project_id: string | null
  repo_id: string | null
  project_name: string | null
  existing_run_id: string | null
  existing_run_status: string | null
  /** All of the user's accounts asked to review this PR (first = `account_id`,
   * the account the Review action runs as). */
  reviewer_accounts: PrInboxReviewerAccount[]
  /** Other pending reviewers (users / teams), excluding the user's accounts.
   * Best-effort — empty when the backend's GraphQL lookup failed. */
  requested_reviewers: PrRequestedReviewer[]
}

/** Mirrors the Rust `PrInboxReviewerAccount`. */
export interface PrInboxReviewerAccount {
  account_id: string
  account_label: string
}

/** Mirrors the Rust `PrRequestedReviewer`. */
export interface PrRequestedReviewer {
  login_or_slug: string
  kind: 'user' | 'team'
  avatar_url: string | null
}

/** Card layout: the full card, or one compact row per PR. */
export type PrInboxDensity = 'comfortable' | 'compact'

/** An account whose review-request fetch failed — mirrors the Rust
 * `PrInboxAccountError`. Surfaced as a warning so a transiently-missing PR
 * reads as "try again" instead of "no PRs". */
export interface PrInboxAccountError {
  account_id: string
  account_label: string
  message: string
}

/** Aggregated inbox result — mirrors the Rust `PrInboxResult`. */
export interface PrInboxResult {
  items: PrInboxItem[]
  errors: PrInboxAccountError[]
}

/** Stable key for a PR across accounts (owner/repo#number). */
export function prKey(item: Pick<PrInboxItem, 'owner' | 'repo' | 'number'>): string {
  return `${item.owner}/${item.repo}#${item.number}`
}

// Inbox view preferences — lasting reading choices, persisted like uiLayout's.
const GROUP_BY_KEY = 'devdy.prInbox.groupBy'
const HIDE_BOTS_KEY = 'devdy.prInbox.hideBots'
const COLLAPSED_KEY = 'devdy.prInbox.collapsedGroups'
const FILTERS_KEY = 'devdy.prInbox.filters'
const DENSITY_KEY = 'devdy.prInbox.density'
const BOT_LOGINS_KEY = 'devdy.prInbox.botLogins'

/** Read + JSON-parse a stored value, or undefined when missing/corrupt. */
function readJson(key: string): unknown {
  try {
    const raw = localStorage.getItem(key)
    return raw == null ? undefined : JSON.parse(raw)
  } catch {
    return undefined
  }
}

function writeStorage(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch {
    /* storage unavailable — keep the in-memory choice */
  }
}

function loadDensity(): PrInboxDensity {
  try {
    if (localStorage.getItem(DENSITY_KEY) === 'compact') return 'compact'
  } catch {
    /* storage unavailable — fall through to the default */
  }
  return 'comfortable'
}

function loadGroupBy(): PrGroupBy {
  try {
    const v = localStorage.getItem(GROUP_BY_KEY)
    if (v && (PR_GROUP_BY_OPTIONS as readonly string[]).includes(v)) return v as PrGroupBy
  } catch {
    /* storage unavailable — fall through to the default */
  }
  return 'project'
}

function loadHideBots(): boolean {
  try {
    // Only the two values setHideBots writes count; anything else → default on.
    const v = localStorage.getItem(HIDE_BOTS_KEY)
    if (v === '0') return false
  } catch {
    /* storage unavailable — fall through to the default */
  }
  return true
}

/** Explicit per-group collapsed overrides (group key → collapsed). Groups not
 * listed fall back to their default (see `isGroupCollapsedByDefault`). */
function loadCollapsed(): Record<string, boolean> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? '{}')
    if (parsed && typeof parsed === 'object' && Object.getPrototypeOf(parsed) === Object.prototype) {
      // Keep only well-formed entries so a hand-edited / stale value can't
      // leak non-boolean collapse states into the view.
      const out: Record<string, boolean> = {}
      for (const [key, value] of Object.entries(parsed)) {
        if (typeof value === 'boolean') out[key] = value
      }
      return out
    }
  } catch {
    /* corrupt or unavailable — start fresh */
  }
  return {}
}

export const usePrInboxStore = defineStore('prInbox', () => {
  const items = ref<PrInboxItem[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)
  // Per-account fetch failures (partial success): the inbox still shows the PRs
  // it did load, but warns that some accounts failed so a missing PR doesn't
  // read as "no PRs".
  const warnings = ref<PrInboxAccountError[]>([])
  const lastRefreshedAt = ref<string | null>(null)
  // Epoch ms of the last refresh ATTEMPT (success or not) — what throttles
  // automatic refreshes, so a failing one (rate limit, network, bad PAT)
  // doesn't re-fire every search on each trigger.
  const lastAttemptAt = ref(0)
  // Keys of PRs currently being turned into a review run (button disabled state).
  const reviewingKeys = ref<Set<string>>(new Set())
  const groupBy = ref<PrGroupBy>(loadGroupBy())
  const hideBots = ref(loadHideBots())
  const collapsedGroups = ref<Record<string, boolean>>(loadCollapsed())
  // Values are validated on load; ones no longer present in the inbox are
  // ignored at filter time (prInboxFilters.effectiveFilters), not deleted.
  const filters = ref<PrFilters>(sanitizeFilters(readJson(FILTERS_KEY)))
  const density = ref<PrInboxDensity>(loadDensity())
  // Extra logins the user treats as bots, on top of GitHub's own bot flag.
  const botLogins = ref<string[]>(sanitizeBotLogins(readJson(BOT_LOGINS_KEY)))

  async function refresh() {
    loading.value = true
    lastAttemptAt.value = Date.now()
    error.value = null
    try {
      const result = await invoke<PrInboxResult>('list_review_requested_prs')
      items.value = result.items
      warnings.value = result.errors
      lastRefreshedAt.value = new Date().toISOString()
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  function setReviewing(key: string, on: boolean) {
    const next = new Set(reviewingKeys.value)
    if (on) next.add(key)
    else next.delete(key)
    reviewingKeys.value = next
  }

  /** Attach a just-created review run to its PR so the button reflects the
   * linked session immediately, without waiting for a refresh. */
  function linkRun(key: string, runId: string, status: string) {
    const item = items.value.find(i => prKey(i) === key)
    if (item) {
      item.existing_run_id = runId
      item.existing_run_status = status
    }
  }

  function setGroupBy(value: PrGroupBy) {
    groupBy.value = value
    try {
      localStorage.setItem(GROUP_BY_KEY, value)
    } catch {
      /* storage unavailable — keep the in-memory choice */
    }
  }

  function setHideBots(value: boolean) {
    hideBots.value = value
    try {
      localStorage.setItem(HIDE_BOTS_KEY, value ? '1' : '0')
    } catch {
      /* storage unavailable — keep the in-memory choice */
    }
  }

  function setGroupCollapsed(key: string, collapsed: boolean) {
    collapsedGroups.value = { ...collapsedGroups.value, [key]: collapsed }
    try {
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify(collapsedGroups.value))
    } catch {
      /* storage unavailable — keep the in-memory choice */
    }
  }

  function setFilters(next: PrFilters) {
    filters.value = sanitizeFilters(next)
    writeStorage(FILTERS_KEY, JSON.stringify(filters.value))
  }

  function clearFilters() {
    setFilters(emptyFilters())
  }

  function setDensity(value: PrInboxDensity) {
    density.value = value
    writeStorage(DENSITY_KEY, value)
  }

  function setBotLogins(logins: string[]) {
    botLogins.value = sanitizeBotLogins(logins)
    writeStorage(BOT_LOGINS_KEY, JSON.stringify(botLogins.value))
  }

  function addBotLogin(login: string) {
    setBotLogins([...botLogins.value, login])
  }

  function removeBotLogin(login: string) {
    const n = normalizeLogin(login)
    setBotLogins(botLogins.value.filter(l => l !== n))
  }

  return {
    items, loading, error, warnings, lastRefreshedAt, lastAttemptAt, reviewingKeys,
    groupBy, hideBots, collapsedGroups, filters, density, botLogins,
    refresh, setReviewing, linkRun, setGroupBy, setHideBots, setGroupCollapsed,
    setFilters, clearFilters, setDensity, setBotLogins, addBotLogin, removeBotLogin,
  }
})
