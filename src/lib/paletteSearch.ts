// Matching + ranking for the command palette (components/CommandPalette.vue).
//
// Pure logic only — unit-tested directly (src/lib/__tests__/paletteSearch.test.mjs).
// Matching is accent-insensitive so a Vietnamese label is found without typing
// its diacritics ("lich" finds "Lịch", "do an" finds "Đồ án").

/** Lower-case, strip combining marks (NFD) and fold đ/Đ to d. */
export function foldText(s: string): string {
  return s
    .normalize('NFD')
    .replace(/\p{M}/gu, '')
    .replace(/[đĐ]/g, 'd')
    .toLowerCase()
}

// How well one query token matched. Higher is better.
const PREFIX = 3
const WORD_START = 2
const SUBSTRING = 1

function isWordChar(ch: string | undefined): boolean {
  return ch !== undefined && /[\p{L}\p{N}]/u.test(ch)
}

/** Score of `token` (already folded) inside `text` (already folded), or 0 if absent. */
function tokenScore(token: string, text: string): number {
  if (text.startsWith(token)) return PREFIX
  let best = 0
  let from = 0
  for (;;) {
    const at = text.indexOf(token, from)
    if (at === -1) return best
    if (!isWordChar(text[at - 1])) return WORD_START
    best = SUBSTRING
    from = at + 1
  }
}

/**
 * Score of `query` against a label (and optional extra keywords), or null when
 * it doesn't match. Every whitespace-separated token must be found; a token
 * found only in the keywords counts as a plain substring match, so the label
 * always wins ties. An empty query matches everything with score 0.
 */
export function matchScore(query: string, label: string, keywords: readonly string[] = []): number | null {
  const tokens = foldText(query).split(/\s+/).filter(Boolean)
  if (tokens.length === 0) return 0
  const foldedLabel = foldText(label)
  const foldedKeywords = keywords.map(foldText)
  let total = 0
  for (const token of tokens) {
    const inLabel = tokenScore(token, foldedLabel)
    if (inLabel > 0) {
      total += inLabel
      continue
    }
    if (foldedKeywords.some((k) => k.includes(token))) {
      total += SUBSTRING
      continue
    }
    return null
  }
  // A whole-query prefix of the label ("pr in" → "PR Inbox") beats the same
  // tokens scattered across it.
  if (foldedLabel.startsWith(tokens.join(' '))) total += 1
  return total
}

export interface PaletteSearchable {
  group: string
  label: string
  keywords?: readonly string[]
}

/**
 * Filter `items` by `query` and order them: groups keep the order given in
 * `groupOrder` (unknown groups last, in first-seen order); inside a group, best
 * score first, ties keeping their original order (e.g. most recent first).
 */
export function rankItems<T extends PaletteSearchable>(
  items: readonly T[],
  query: string,
  groupOrder: readonly string[] = [],
): T[] {
  const scored: { item: T; score: number; index: number }[] = []
  items.forEach((item, index) => {
    const score = matchScore(query, item.label, item.keywords)
    if (score !== null) scored.push({ item, score, index })
  })
  const rank = new Map<string, number>()
  groupOrder.forEach((g, i) => rank.set(g, i))
  for (const { item } of scored) {
    if (!rank.has(item.group)) rank.set(item.group, rank.size + groupOrder.length)
  }
  scored.sort(
    (a, b) =>
      rank.get(a.item.group)! - rank.get(b.item.group)! || b.score - a.score || a.index - b.index,
  )
  return scored.map((s) => s.item)
}
