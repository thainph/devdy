import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'

/** Snapshot of the local Inbox API (Settings → Inbox API). */
export interface InboxApiInfo {
  enabled: boolean
  running: boolean
  port: number | null
  token: string
  baseUrl: string | null
}

/** Commands of the loopback Inbox API the Chrome extension talks to. */
export const useInboxApiStore = defineStore('inboxApi', () => {
  async function getInboxApiInfo(): Promise<InboxApiInfo> {
    return invoke<InboxApiInfo>('get_inbox_api_info')
  }

  async function regenerateInboxApiToken(): Promise<InboxApiInfo> {
    return invoke<InboxApiInfo>('regenerate_inbox_api_token')
  }

  /** Start or stop the whole Inbox API server (persisted). */
  async function setInboxApiEnabled(enabled: boolean): Promise<InboxApiInfo> {
    return invoke<InboxApiInfo>('set_inbox_api_enabled', { enabled })
  }

  return { getInboxApiInfo, regenerateInboxApiToken, setInboxApiEnabled }
})
