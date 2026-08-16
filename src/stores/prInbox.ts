import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

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
  updated_at: string
  mapped: boolean
  project_id: string | null
  repo_id: string | null
  project_name: string | null
  existing_run_id: string | null
  existing_run_status: string | null
}

/** Stable key for a PR across accounts (owner/repo#number). */
export function prKey(item: Pick<PrInboxItem, 'owner' | 'repo' | 'number'>): string {
  return `${item.owner}/${item.repo}#${item.number}`
}

export const usePrInboxStore = defineStore('prInbox', () => {
  const items = ref<PrInboxItem[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)
  const lastRefreshedAt = ref<string | null>(null)
  // Keys of PRs currently being turned into a review run (button disabled state).
  const reviewingKeys = ref<Set<string>>(new Set())

  async function refresh() {
    loading.value = true
    error.value = null
    try {
      items.value = await invoke<PrInboxItem[]>('list_review_requested_prs')
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

  return { items, loading, error, lastRefreshedAt, reviewingKeys, refresh, setReviewing, linkRun }
})
