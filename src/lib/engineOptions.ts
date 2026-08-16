// Pure data tables for the run composer's engine/model/permission selectors.
//
// Extracted from RunView so both the desktop app AND the remote controller
// (which must stay Tauri-free) can import the same option lists. Keep this file
// dependency-free: no Tauri, no Pinia, no Vue — just data + tiny helpers.

export interface SelectOption {
  value: string
  label: string
}

// Model choices depend on the engine. Empty value = let the engine/setting decide.
export const MODEL_OPTIONS: Record<string, SelectOption[]> = {
  claude: [
    { value: '', label: 'Default (from settings)' },
    // `[1m]` selects the 1M-context variant; the bare alias uses the 200K default.
    { value: 'fable', label: 'Fable 5 (1M)' },
    { value: 'opus', label: 'Opus (200K)' },
    { value: 'opus[1m]', label: 'Opus (1M)' },
    { value: 'sonnet', label: 'Sonnet (200K)' },
    { value: 'sonnet[1m]', label: 'Sonnet (1M)' },
    { value: 'haiku', label: 'Haiku' },
  ],
  codex: [
    { value: '', label: 'Default (from settings)' },
    { value: 'gpt-5.5', label: 'gpt-5.5' },
    { value: 'gpt-5.4', label: 'gpt-5.4' },
    { value: 'gpt-5.3-codex', label: 'gpt-5.3-codex' },
    { value: 'gpt-5.2-codex', label: 'gpt-5.2-codex' },
    { value: 'gpt-5.1-codex-mini', label: 'gpt-5.1-codex-mini' },
  ],
}

export const PERMISSION_MODE_OPTIONS: SelectOption[] = [
  { value: '', label: 'Default (from settings)' },
  { value: 'default', label: 'Ask via UI (default)' },
  { value: 'acceptEdits', label: 'Auto-accept edits' },
  { value: 'plan', label: 'Plan only (read-only)' },
  { value: 'auto', label: 'Auto (classifier)' },
  { value: 'bypassPermissions', label: 'Bypass all' },
]

/** Model options for an engine, falling back to Claude's list. */
export function modelOptionsFor(engine: string): SelectOption[] {
  return MODEL_OPTIONS[engine] ?? MODEL_OPTIONS.claude
}
