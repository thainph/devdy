// Pure data tables for the run composer's engine/model/permission selectors.
//
// Extracted from RunView so both the desktop app AND the remote controller
// (which must stay Tauri-free) can import the same option lists. Keep this file
// dependency-free: no Tauri, no Pinia, no Vue — just data + tiny helpers.

export interface SelectOption {
  value: string
  label: string
  /** Dimmed second line in the dropdown (AppSelect renders it). */
  description?: string
}

/** A row in the unified model picker: a concrete model carries the engine it
 * runs on; `header` rows are non-selectable group titles ("Claude" / "Codex"). */
export interface CombinedOption extends SelectOption {
  engine?: string
  header?: boolean
}

// Model choices depend on the engine. Empty value = let the engine/setting decide.
//
// The Claude list is PINNED to exact wire ids rather than family aliases
// (`opus`, `sonnet`, …): an alias silently follows whatever version ships next,
// which is precisely what this list exists to avoid. `[1m]` opts the run into
// the 1M-context beta; the bare id runs at 200K.
//
// The CLI accepts far more ids than its own picker advertises, so this table
// cannot be discovered — it is curated, and the Settings screen's Validate
// button is how you confirm the account still serves each entry.
export const MODEL_OPTIONS: Record<string, SelectOption[]> = {
  claude: [
    { value: '', label: 'Default (from settings)' },
    { value: 'claude-opus-5-5', label: 'claude-opus-5-5', description: 'Opus 5.5 · 200K context' },
    { value: 'claude-opus-5-5[1m]', label: 'claude-opus-5-5[1m]', description: 'Opus 5.5 · 1M context' },
    { value: 'claude-opus-4-8', label: 'claude-opus-4-8', description: 'Opus 4.8 · 200K context' },
    { value: 'claude-opus-4-8[1m]', label: 'claude-opus-4-8[1m]', description: 'Opus 4.8 · 1M context' },
    { value: 'claude-sonnet-4-6', label: 'claude-sonnet-4-6', description: 'Sonnet 4.6 · 200K context' },
    { value: 'claude-sonnet-4-6[1m]', label: 'claude-sonnet-4-6[1m]', description: 'Sonnet 4.6 · 1M context' },
    // Fable ships 1M as both the default and the maximum — no 200K variant.
    { value: 'claude-fable-5-1', label: 'claude-fable-5-1', description: 'Fable 5.1 · 1M context' },
    { value: 'claude-haiku-4-5', label: 'claude-haiku-4-5', description: 'Haiku 4.5 · 200K context' },
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

/**
 * Flatten the per-engine Claude + Codex lists into ONE grouped picker, so the
 * composer needs a single model pick and no separate engine selector — the
 * engine is implied by the chosen model. The leading entry is the global Default
 * (empty value + empty engine = follow settings); each group is preceded by a
 * non-selectable header. Callers pass lists already annotated for their context
 * (validation marks on desktop, discovered Codex models on both).
 */
export function combinedModelOptions(args: {
  claude: SelectOption[]
  codex: SelectOption[]
  defaultLabel: string
  claudeLabel: string
  codexLabel: string
}): CombinedOption[] {
  const strip = (opts: SelectOption[]) => opts.filter((o) => o.value !== '')
  const out: CombinedOption[] = [{ value: '', engine: '', label: args.defaultLabel }]
  const claude = strip(args.claude)
  if (claude.length) {
    out.push({ value: '__group:claude', label: args.claudeLabel, header: true })
    for (const o of claude) out.push({ ...o, engine: 'claude' })
  }
  const codex = strip(args.codex)
  if (codex.length) {
    out.push({ value: '__group:codex', label: args.codexLabel, header: true })
    for (const o of codex) out.push({ ...o, engine: 'codex' })
  }
  return out
}

/** Engine a picked model belongs to, looked up in a combined list. Empty value
 * (Default) returns '' — the caller then falls back to the global default. */
export function engineForModel(model: string, opts: CombinedOption[]): string {
  if (!model) return ''
  return opts.find((o) => !o.header && o.value === model)?.engine || ''
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

/** All Claude model ids the Validate button checks — the pinned table minus the
 * "Default (from settings)" placeholder, which names no model. */
export const CLAUDE_MODEL_IDS: string[] = MODEL_OPTIONS.claude
  .map((o) => o.value)
  .filter(Boolean)

/**
 * Keep an already-saved model id selectable even when it is no longer in the
 * table — a family alias from an older build, or a version since dropped.
 * Without this the selector silently rewrites the user's stored choice the
 * first time they open the screen.
 */
export function withCurrentModel(
  options: SelectOption[],
  current: string | null | undefined,
): SelectOption[] {
  const id = current?.trim()
  if (!id || options.some((o) => o.value === id)) return options
  return [...options, { value: id, label: id, description: 'No longer offered' }]
}

/** A model discovered from the account (`codex debug models`). Claude's list is
 * curated, not discovered — see MODEL_OPTIONS above. */
export interface FetchedModel {
  value: string
  label: string
  description?: string
}

/**
 * Codex options: a discovered list is authoritative (it comes from the CLI), so
 * keep only the leading "Default" option and replace the curated models with it.
 * With nothing discovered, return the curated fallback unchanged.
 */
export function codexOptions(base: SelectOption[], fetched: FetchedModel[]): SelectOption[] {
  if (!fetched.length) return base
  const defaultOpt = base.find((o) => o.value === '') ?? { value: '', label: base[0]?.label ?? '' }
  return [
    defaultOpt,
    ...fetched.map((m) =>
      m.description ? { value: m.value, label: m.label, description: m.description } : { value: m.value, label: m.label },
    ),
  ]
}

/** The effective model list for an engine. Codex is discovered from its CLI;
 * Claude is the curated pinned table. This is what both the desktop composer and
 * the remote controller render, so the two never disagree about what is offered. */
export function effectiveModelOptions(
  engine: string,
  discovered: { codex: FetchedModel[] },
): SelectOption[] {
  const base = modelOptionsFor(engine)
  if (engine === 'codex') return codexOptions(base, discovered.codex)
  // Claude is the fallback engine, so anything unknown gets its table too.
  return base
}
