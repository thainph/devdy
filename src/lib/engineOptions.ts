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

/** A model discovered from the account (Agent SDK `supportedModels()` for
 * Claude, `codex debug models` for Codex) rather than from the curated table. */
export interface FetchedModel {
  value: string
  label: string
  description?: string
}

/**
 * Merge curated base options with discovered Claude models that aren't already
 * covered. A discovered id is "already covered" when present verbatim, or when
 * it belongs to a family an alias already represents (e.g. `opus` covers
 * `claude-opus-4-5`). Only genuinely new families are appended, so the curated
 * aliases — and the `[1m]` context-limit math that depends on them — survive.
 */
export function mergedClaudeOptions(
  base: SelectOption[],
  fetched: FetchedModel[],
): SelectOption[] {
  const knownValues = new Set(base.map((o) => o.value))
  const aliasStems = base
    .map((o) => o.value.replace(/\[.*\]$/, '').toLowerCase())
    .filter(Boolean)
  const extras = fetched
    .filter((m) => {
      if (knownValues.has(m.value)) return false
      const id = m.value.toLowerCase()
      return !aliasStems.some((stem) => id.includes(stem))
    })
    .map((m) => ({ value: m.value, label: m.label }))
  return extras.length ? [...base, ...extras] : base
}

/**
 * Codex options: a discovered list is authoritative (it comes from the CLI), so
 * keep only the leading "Default" option and replace the curated models with it.
 * With nothing discovered, return the curated fallback unchanged.
 */
export function codexOptions(base: SelectOption[], fetched: FetchedModel[]): SelectOption[] {
  if (!fetched.length) return base
  const defaultOpt = base.find((o) => o.value === '') ?? { value: '', label: base[0]?.label ?? '' }
  return [defaultOpt, ...fetched.map((m) => ({ value: m.value, label: m.label }))]
}

/** The effective model list for an engine: curated table + whatever the Host
 * discovered for the account. This is what both the desktop composer and the
 * remote controller must render, so they never disagree about what is on offer. */
export function effectiveModelOptions(
  engine: string,
  discovered: { claude: FetchedModel[]; codex: FetchedModel[] },
): SelectOption[] {
  const base = modelOptionsFor(engine)
  if (engine === 'codex') return codexOptions(base, discovered.codex)
  // Claude is the fallback engine, so anything unknown gets its treatment too.
  return mergedClaudeOptions(base, discovered.claude)
}
