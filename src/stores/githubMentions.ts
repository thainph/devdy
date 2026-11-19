import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import {
  categoryOf,
  EMPTY_MENTION_FILTERS,
  MENTION_CATEGORIES,
  MENTION_GROUP_BYS,
  sanitizeFilters,
  type CachedMentions,
  type MentionAccountError,
  type MentionCategory,
  type MentionFilterDimension,
  type MentionFilters,
  type MentionGroupBy,
  type MentionItem,
  type MentionMute,
  type MentionMuteScope,
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
const TAB_KEY = 'devdy.githubMentions.tab'

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

function initialTab(): MentionCategory {
  const v = readStorage(TAB_KEY)
  return MENTION_CATEGORIES.includes(v as MentionCategory) ? (v as MentionCategory) : 'reply'
}

export const useGithubMentionsStore = defineStore('githubMentions', () => {
  const items = ref<MentionItem[]>([])
  const warnings = ref<MentionAccountError[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)
  const fetchedAt = ref<string | null>(null)
  const filters = ref<MentionFilters>(initialFilters())
  const groupBy = ref<MentionGroupBy>(initialGroupBy())
  /** Days of notifications looked at (backend setting, default 3). */
  const lookbackDays = ref(3)
  /** Keys with a request in flight (starting an AI check / dismiss), to disable their buttons. */
  const busyKeys = ref<Set<string>>(new Set())
  /** Muted threads / repos ("Not interested"), newest first. */
  const mutes = ref<MentionMute[]>([])

  /** Category shown (reply / fyi / done). */
  const tab = ref<MentionCategory>(initialTab())
  /** Background AI sorting (backend setting, on by default). */
  const aiTriage = ref(true)

  /** Mentions that need an answer (sidebar badge); FYI and settled ones don't count. */
  const pendingCount = computed(() => items.value.filter(i => categoryOf(i) === 'reply').length)
  /** The AI is sorting some mentions right now. */
  const triaging = computed(() => items.value.filter(i => i.triage_pending).length)

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
  async function loadLookback() {
    try {
      lookbackDays.value = await invoke<number>('get_github_mentions_lookback_days')
    } catch {
      /* keep the default */
    }
  }

  /** Change the window and fetch again with it. */
  async function setLookbackDays(days: number) {
    const prev = lookbackDays.value
    lookbackDays.value = days
    try {
      await invoke('set_github_mentions_lookback_days', { days })
    } catch (e) {
      lookbackDays.value = prev
      throw e
    }
    items.value = []
    fetchedAt.value = null
    await refresh()
  }

  async function load() {
    void listenUpdates()
    void loadLookback()
    void loadAiTriage()
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

  /** Remember the AI-check session started for a mention (the backend
   * patches its cache and broadcasts, which updates `items`). */
  function linkRun(key: string, runId: string) {
    return invoke('link_github_mention_run', { key, runId })
  }

  function dismiss(key: string) {
    return withBusy(key, () => invoke('dismiss_github_mention', { key }))
  }

  async function loadMutes() {
    try {
      mutes.value = await invoke<MentionMute[]>('list_github_mention_mutes')
    } catch {
      /* keep the last list */
    }
  }

  /** Never list this thread / its repo again (the backend drops it from the cache). */
  async function mute(key: string, scope: MentionMuteScope) {
    await withBusy(key, () => invoke('mute_github_mention', { key, scope }))
    await loadMutes()
  }

  /** Unmute and fetch again so the thread / repo comes back. */
  async function unmute(m: MentionMute) {
    await invoke('unmute_github_mention', { scope: m.scope, target: m.target })
    mutes.value = mutes.value.filter(x => !(x.scope === m.scope && x.target === m.target))
    await refresh()
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

  function setTab(value: MentionCategory) {
    tab.value = value
    writeStorage(TAB_KEY, value)
  }

  /** Move a mention to another category by hand; null lets Devdy sort it again. */
  function setCategory(key: string, category: MentionCategory | null) {
    return withBusy(key, () => invoke('set_github_mention_category', { key, category }))
  }

  async function loadAiTriage() {
    try {
      aiTriage.value = await invoke<boolean>('get_github_mentions_ai_triage')
    } catch {
      /* keep the default */
    }
  }

  async function setAiTriage(enabled: boolean) {
    const prev = aiTriage.value
    aiTriage.value = enabled
    try {
      await invoke('set_github_mentions_ai_triage', { enabled })
    } catch (e) {
      aiTriage.value = prev
      throw e
    }
  }

  function setGroupBy(value: MentionGroupBy) {
    groupBy.value = value
    writeStorage(GROUP_BY_KEY, value)
  }

  /** The "Check with AI" instruction: saved prompt, custom text or the default. */
  function loadCheckPrompt(): Promise<string> {
    return invoke<string>('get_github_mentions_check_prompt')
  }

  /** The prompt editor's view: linked saved prompt id (if any) + custom text (else the default). */
  function loadPromptConfig(): Promise<{ saved_prompt_id: string | null; prompt: string }> {
    return invoke('get_github_mentions_prompt_config')
  }

  /** Save the custom check prompt; blank restores the default. */
  async function savePrompt(prompt: string): Promise<void> {
    await invoke('set_github_mentions_prompt', { prompt })
  }

  /** Use a Settings → Prompt Templates entry; null goes back to the custom text. */
  async function savePromptId(savedPromptId: string | null): Promise<void> {
    await invoke('set_github_mentions_prompt_id', { savedPromptId })
  }

  return {
    loadCheckPrompt, loadPromptConfig, savePrompt, savePromptId,
    items, warnings, loading, error, fetchedAt, filters, groupBy, lookbackDays, setLookbackDays, busyKeys, pendingCount,
    mutes, loadMutes, mute, unmute,
    tab, setTab, setCategory, triaging, aiTriage, loadAiTriage, setAiTriage,
    load, watchCache, refresh, linkRun, withBusy, dismiss, markRead, setFilter, clearFilters, setGroupBy,
  }
})
