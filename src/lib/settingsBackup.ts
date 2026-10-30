// Wire types for Settings → Backup & Sync (file backup). Mirrors
// `src-tauri/src/commands/settings_backup.rs`.

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
  'claudeAccounts', 'notes', 'todos', 'slackThreads', 'projects',
] as const

export function formatExportedAt(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}
