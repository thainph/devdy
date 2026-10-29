// Wire types shared by Settings → Backup & Sync (file backup) and LAN sync. Mirrors
// `src-tauri/src/commands/settings_backup.rs` and `src-tauri/src/lan_sync/mod.rs`.

export interface BackupInfo {
  exportedAt: string
  appVersion: string
  hasSecrets: boolean
  counts: Record<string, number>
}

export interface ImportCounts {
  created: number
  updated: number
  unchanged: number
}

export interface ImportReport {
  counts: Record<string, ImportCounts>
  secretsImported: boolean
  warnings: string[]
}

export const BACKUP_SECTION_KEYS = [
  'settings', 'skills', 'rules', 'skillGroups', 'ruleGroups', 'mcpServers', 'vpsServers',
  'githubAccounts', 'gitlabAccounts', 'awsAccounts', 'azureAccounts', 'googleAccounts',
  'claudeAccounts', 'notes', 'todos', 'slackThreads', 'projects', 'pulseWatches',
] as const

export interface LanShareStatus {
  code: string
  port: number
  endpoints: string[]
  deviceName: string
  expiresAt: string
  includeSecrets: boolean
  advertised: boolean
}

export interface LanShareEnded {
  reason: 'sent' | 'expired' | 'stopped' | 'tooManyAttempts'
  peer: string | null
}

export interface LanDevice {
  id: string
  name: string
  appVersion: string
  endpoints: string[]
}

export interface LanFetchResult {
  peerName: string
  info: BackupInfo
}

export const LAN_SHARE_ENDED_EVENT = 'lan_sync_share_ended'

export function formatExportedAt(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}

/** `123456` → `123 456` for display. */
export function formatShareCode(code: string) {
  return code.length === 6 ? `${code.slice(0, 3)} ${code.slice(3)}` : code
}
