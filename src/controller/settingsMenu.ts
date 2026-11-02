/**
 * The Controller's Setting tab is a menu; each entry opens its own screen.
 * Project switch screens are a {@link ToggleGroup}; the rest belong to this
 * phone (look, notifications).
 */
import type { ToggleKind } from './protocol'

export type ToggleGroup = 'mcp' | 'skill' | 'rule' | 'server' | 'account'

/** The switch kinds each project screen shows. */
export const TOGGLE_GROUPS: Record<ToggleGroup, readonly ToggleKind[]> = {
  mcp: ['mcp', 'builtin_mcp'],
  skill: ['skill'],
  rule: ['rule'],
  server: ['server'],
  account: ['claude_account', 'github_account', 'gitlab_account', 'aws_account', 'azure_account'],
}

export type SettingsPage = ToggleGroup | 'theme' | 'push'

export function isToggleGroup(page: SettingsPage | null): page is ToggleGroup {
  return page !== null && page in TOGGLE_GROUPS
}
