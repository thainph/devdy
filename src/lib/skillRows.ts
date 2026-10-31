// Pure helpers behind the Skills list-detail screen: the sidebar rows (search,
// group filter, folders) and the SKILL.md frontmatter handling of the editor.

import { GROUP_NONE, haystack, matchesSearch, type RowGroup } from './listGroups.ts'

export type SkillTarget = 'claude' | 'codex' | 'both'

export interface SkillLike {
  id: string
  name: string
  description: string
  target: SkillTarget
}

export interface GroupLike {
  id: string
  name: string
}

/** Group filter value of the "Ungrouped" option ('' = all groups). */
export const FILTER_UNGROUPED = '__ungrouped__'

export interface SkillRow<S extends SkillLike = SkillLike> {
  id: string
  skill: S
  /** The skill's other groups (besides the folder the row sits in). */
  otherGroupIds: string[]
}

export function skillMatches(skill: SkillLike, query: string): boolean {
  return matchesSearch(haystack(skill.name, skill.description), query)
}

/**
 * Sidebar folders. Groups are many-to-many, so a skill in several groups is
 * listed under each of them (like a tag) — hiding it from all but one would
 * make the others look incomplete. Folders follow the groups' own order
 * (the order set in the group manager), "Ungrouped" last, empty folders
 * dropped. Group ids no longer in `groups` are ignored.
 */
export function buildSkillGroups<S extends SkillLike>(
  skills: readonly S[],
  itemGroups: Readonly<Record<string, readonly string[]>>,
  groups: readonly GroupLike[],
  opts: { search: string; group: string; ungroupedLabel: string },
): RowGroup<SkillRow<S>>[] {
  const known = new Set(groups.map((g) => g.id))
  const groupsOf = (id: string) => (itemGroups[id] ?? []).filter((g) => known.has(g))
  const matching = skills.filter((s) => skillMatches(s, opts.search))

  const out: RowGroup<SkillRow<S>>[] = []
  for (const g of groups) {
    if (opts.group && opts.group !== g.id) continue
    const rows = matching
      .filter((s) => groupsOf(s.id).includes(g.id))
      .map((s) => ({ id: s.id, skill: s, otherGroupIds: groupsOf(s.id).filter((x) => x !== g.id) }))
    if (rows.length) out.push({ key: g.id, label: g.name, rows })
  }
  if (!opts.group || opts.group === FILTER_UNGROUPED) {
    const rows = matching
      .filter((s) => groupsOf(s.id).length === 0)
      .map((s) => ({ id: s.id, skill: s, otherGroupIds: [] }))
    if (rows.length) out.push({ key: GROUP_NONE, label: opts.ungroupedLabel, rows })
  }
  return out
}

// ── SKILL.md frontmatter ───────────────────────────────────────────────────

export const SKILL_TEMPLATE = `---
name: my-skill
description: What this skill does
target: both
---

# My Skill

Describe what this skill does.
`

export interface Frontmatter {
  name: string
  description: string
  target: string
}

/** An i18n key under `skills.editor` plus its params. */
export interface FrontmatterError {
  key: string
  params?: Record<string, string>
}

function unquote(v: string): string {
  if (v.length >= 2 && v.startsWith('"') && v.endsWith('"')) {
    return v.slice(1, -1).replace(/\\(["\\])/g, '$1')
  }
  if (v.length >= 2 && v.startsWith("'") && v.endsWith("'")) return v.slice(1, -1).replace(/''/g, "'")
  return v
}

export function parseFrontmatter(raw: string): Frontmatter {
  if (!raw.startsWith('---')) return { name: '', description: '', target: '' }
  const rest = raw.slice(3)
  const end = rest.indexOf('---')
  if (end === -1) return { name: '', description: '', target: '' }
  const fm = rest.slice(0, end)
  const pick = (key: string) => unquote(fm.match(new RegExp(`^${key}:\\s*(.+)$`, 'm'))?.[1]?.trim() ?? '')
  return { name: pick('name'), description: pick('description'), target: pick('target') }
}

// A plain (unquoted) YAML scalar must not start with an indicator char, contain
// `: ` / ` #`, or end with a colon — otherwise it needs double-quoting.
export function isPlainSafe(v: string): boolean {
  if (v === '' || /^[\s!&*?|>%@`"'#,[\]{}:-]/.test(v)) return false
  if (/:(\s|$)/.test(v) || /\s#/.test(v) || /[\n\t]/.test(v)) return false
  return true
}

export function yamlQuote(v: string): string {
  return '"' + v.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"'
}

export function yamlScalar(v: string): string {
  return isPlainSafe(v) ? v : yamlQuote(v)
}

/**
 * Collapse any frontmatter value that was wrapped onto multiple unquoted lines
 * back into a single valid line. Returns the (possibly rewritten) content plus
 * a hard error when a line can't be interpreted (block save in that case).
 */
export function normalizeFrontmatter(raw: string): { content: string; changed: boolean; error: FrontmatterError | null } {
  const lines = raw.split('\n')
  if (lines[0]?.trim() !== '---') return { content: raw, changed: false, error: null }
  let end = -1
  for (let i = 1; i < lines.length; i++) {
    if (lines[i].trim() === '---') { end = i; break }
  }
  if (end === -1) return { content: raw, changed: false, error: { key: 'errFrontmatterMissingClose' } }

  const keyRe = /^([A-Za-z0-9_-]+):[ \t]?(.*)$/
  const out: string[] = []
  let changed = false
  let i = 1
  while (i < end) {
    const line = lines[i]
    if (line.trim() === '' || line.trimStart().startsWith('#')) { out.push(line); i++; continue }
    const km = line.match(keyRe)
    if (!km) return { content: raw, changed: false, error: { key: 'errInvalidLine', params: { line: line.trim() } } }
    const key = km[1]
    let value = km[2]
    // Block scalars (| or >) are already valid YAML — copy them and their indented body verbatim.
    if (/^[|>]/.test(value.trim())) {
      out.push(line); i++
      while (i < end && (lines[i].trim() === '' || /^\s/.test(lines[i]))) { out.push(lines[i]); i++ }
      continue
    }
    // Gather continuation lines: non-blank lines that don't start a new key.
    let j = i + 1
    const cont: string[] = []
    while (j < end && lines[j].trim() !== '' && !keyRe.test(lines[j])) { cont.push(lines[j].trim()); j++ }
    if (cont.length) {
      value = [value.trim(), ...cont].join(' ').trim()
      out.push(`${key}: ${yamlScalar(value)}`)
      changed = true
      i = j
    } else {
      out.push(line); i++
    }
  }

  if (!changed) return { content: raw, changed: false, error: null }
  const rebuilt = ['---', ...out, '---', ...lines.slice(end + 1)].join('\n')
  return { content: rebuilt, changed: true, error: null }
}

export function validateFrontmatter(raw: string): FrontmatterError | null {
  const norm = normalizeFrontmatter(raw)
  if (norm.error) return norm.error
  const fm = parseFrontmatter(norm.content)
  if (!fm.name) return { key: 'errMissingName' }
  if (!/^[a-zA-Z0-9_-]+$/.test(fm.name)) return { key: 'errNameSlug' }
  if (!fm.description) return { key: 'errMissingDescription' }
  if (fm.target && !['claude', 'codex', 'both'].includes(fm.target)) return { key: 'errTargetInvalid' }
  return null
}

/**
 * Rewrite (or add) one top-level frontmatter key — the metadata fields edit
 * SKILL.md through this, so the file stays the single source of truth. A file
 * without frontmatter gets one.
 */
export function setFrontmatterField(raw: string, key: string, value: string): string {
  const line = `${key}: ${yamlScalar(value)}`
  const lines = raw.split('\n')
  if (lines[0]?.trim() !== '---') return `---\n${line}\n---\n\n${raw}`
  let end = -1
  for (let i = 1; i < lines.length; i++) {
    if (lines[i].trim() === '---') { end = i; break }
  }
  if (end === -1) return raw
  const keyRe = new RegExp(`^${key}:`)
  const at = lines.findIndex((l, i) => i > 0 && i < end && keyRe.test(l))
  if (at === -1) {
    lines.splice(end, 0, line)
    return lines.join('\n')
  }
  // Drop the value's continuation lines (a wrapped or block scalar) with it.
  let stop = at + 1
  while (stop < end && lines[stop].trim() !== '' && !/^[A-Za-z0-9_-]+:/.test(lines[stop])) stop++
  lines.splice(at, stop - at, line)
  return lines.join('\n')
}
