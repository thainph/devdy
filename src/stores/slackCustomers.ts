import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'

/**
 * Slack users the user marked as customers (`slack_customers`); their
 * messages carry a "Customer" badge in the Slack threads window, where they
 * are marked; listed in Settings. Every window has its own Pinia, so writes
 * are broadcast (`slack-customers://changed`) and each window's store reloads.
 */

/** Mirrors the Rust `SlackCustomer`. */
export interface SlackCustomer {
  teamId: string
  userId: string
  projectId: string | null
  note: string
  createdAt: string
  displayName: string
  realName: string | null
  /** Absolute path of the cached avatar. */
  avatarPath: string | null
  workspace: string | null
}

export const SLACK_CUSTOMERS_CHANGED = 'slack-customers://changed'

export function customerKey(teamId: string, userId: string): string {
  return `${teamId}/${userId}`
}

export const useSlackCustomersStore = defineStore('slackCustomers', () => {
  const customers = ref<SlackCustomer[]>([])
  const loaded = ref(false)
  const error = ref<string | null>(null)

  const keys = computed(() => new Set(customers.value.map((c) => customerKey(c.teamId, c.userId))))

  function isCustomer(teamId: string | null | undefined, userId: string | null | undefined): boolean {
    return !!teamId && !!userId && keys.value.has(customerKey(teamId, userId))
  }

  async function load() {
    try {
      customers.value = await invoke<SlackCustomer[]>('list_slack_customers')
      error.value = null
      loaded.value = true
    } catch (e) {
      error.value = String(e)
    }
  }

  let listening = false
  /** Load once and follow every window's writes. */
  async function start() {
    if (!loaded.value) void load()
    if (listening) return
    listening = true
    try {
      await listen(SLACK_CUSTOMERS_CHANGED, () => void load())
    } catch {
      /* outside the Tauri shell */
    }
  }

  async function mark(teamId: string, userId: string, projectId: string | null = null, note = '') {
    await invoke('set_slack_customer', { teamId, userId, projectId, note })
    await load()
  }

  async function unmark(teamId: string, userId: string) {
    await invoke('remove_slack_customer', { teamId, userId })
    await load()
  }

  return { customers, loaded, error, isCustomer, load, start, mark, unmark }
})
