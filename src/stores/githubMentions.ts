import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import {
  EMPTY_MENTION_FILTERS,
  MENTION_GROUP_BYS,
  sanitizeFilters,
  type CachedMentions,
  type MentionAccountError,
  type MentionFilterDimension,
  type MentionFilters,
  type MentionGroupBy,
  type MentionItem,
  type MentionsResult,
} from '@/lib/githubMentions'

// GitHub Mentions (views/GithubMentionsView): the backend
// (`src-tauri/src/github_mentions`) caches and polls; this store shows the
// cache at once and follows its `github-mentions-updated` broadcasts, so the
// sidebar badge stays current without the screen being open.

const UPDATED_EVENT = 'github-mentions-updated'
const CACHE_FRESH_MS = 60_000
const FILTERS_KEY = 'devdy.githubMentions.filters'
const GROUP_BY_KEY = 'devdy.githubMentions.groupBy'

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function writeStorage(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch {
    /* storage unavailable — the choice lasts for this session only */
  }
}

function initialFilters(): MentionFilters {
  try {
    return sanitizeFilters(JSON.parse(readStorage(FILTERS_KEY) ?? 'null'))
  } catch {
    return { ...EMPTY_MENTION_FILTERS }
  }
}

function initialGroupBy(): MentionGroupBy {
  const v = readStorage(GROUP_BY_KEY)
  return MENTION_GROUP_BYS.includes(v as MentionGroupBy) ? (v as MentionGroupBy) : 'repo'
}

export const useGithubMentionsStore = defineStore('githubMentions', () => {
  const items = ref<MentionItem[]>([])
  const warnings = ref<MentionAccountError[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)
  const fetchedAt = ref<string | null>(null)
  const filters = ref<MentionFilters>(initialFilters())
  const groupBy = ref<MentionGroupBy>(initialGroupBy())
  /** Keys with a request in flight (reply / dismiss), to disable their buttons. */
  const busyKeys = ref<Set<string>>(new Set())

  /** Mentions waiting for an answer (sidebar badge). */
  const pendingCount = computed(() => items.value.filter(i => !i.answered).length)

  function apply(c: CachedMentions) {
    items.value = c.items
    warnings.value = c.errors
    fetchedAt.value = c.fetched_at
  }

  let listening = false
  async function listenUpdates() {
    if (listening) return
    listening = true
    try {
      await listen<CachedMentions>(UPDATED_EVENT, e => apply(e.payload))
    } catch {
      listening = false
    }
  }

  /** Show the cached list at once; re-fetch when it is missing or older than a minute. */
  async function load() {
    void listenUpdates()
    if (!fetchedAt.value) {
      loading.value = true
      try {
        const cached = await invoke<CachedMentions | null>('get_cached_github_mentions')
        if (cached) apply(cached)
      } catch {
        /* no cache yet */
      } finally {
        loading.value = false
      }
    }
    const at = fetchedAt.value ? Date.parse(fetchedAt.value) : 0
    if (Date.now() - at > CACHE_FRESH_MS) await refresh()
  }

  /** Badge only: follow the backend cache without fetching. */
  async function watchCache() {
    void listenUpdates()
    try {
      const cached = await invoke<CachedMentions | null>('get_cached_github_mentions')
      if (cached && !fetchedAt.value) apply(cached)
    } catch {
      /* the first poll will broadcast */
    }
  }

  async function refresh() {
    loading.value = true
    error.value = null
    try {
      const r = await invoke<MentionsResult>('list_github_mentions')
      items.value = r.items
      warnings.value = r.errors
      fetchedAt.value = new Date().toISOString()
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function withBusy<T>(key: string, f: () => Promise<T>): Promise<T> {
    busyKeys.value = new Set(busyKeys.value).add(key)
    try {
      return await f()
    } finally {
      const next = new Set(busyKeys.value)
      next.delete(key)
      busyKeys.value = next
    }
  }

  /** The backend patches its cache and broadcasts, which updates `items`. */
  function reply(key: string, body: string) {
    return withBusy(key, () => invoke<{ html_url: string }>('reply_github_mention', { key, body }))
  }

  function draft(key: string) {
    return invoke<string>('draft_github_mention_reply', { key })
  }

  function dismiss(key: string) {
    return withBusy(key, () => invoke('dismiss_github_mention', { key }))
  }

  function markRead(key: string) {
    return invoke('mark_github_mention_read', { key })
  }

  function setFilter(dimension: MentionFilterDimension, values: string[]) {
    filters.value = { ...filters.value, [dimension]: values }
    writeStorage(FILTERS_KEY, JSON.stringify(filters.value))
  }

  function clearFilters() {
    filters.value = { ...EMPTY_MENTION_FILTERS }
    writeStorage(FILTERS_KEY, JSON.stringify(filters.value))
  }

  function setGroupBy(value: MentionGroupBy) {
    groupBy.value = value
    writeStorage(GROUP_BY_KEY, value)
  }

  return {
    items, warnings, loading, error, fetchedAt, filters, groupBy, busyKeys, pendingCount,
    load, watchCache, refresh, reply, draft, dismiss, markRead, setFilter, clearFilters, setGroupBy,
  }
})
