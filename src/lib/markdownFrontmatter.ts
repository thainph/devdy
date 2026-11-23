// YAML frontmatter in a markdown preview. Handed to markdown-it as-is, the
// closing `---` turns the block above it into a setext heading, so the file
// viewer splits it off and renders it on its own (like GitHub does).

export interface SplitFrontmatter {
  /** The raw YAML between the fences, or null when the file has none. */
  frontmatter: string | null
  /** The markdown after the closing fence. */
  body: string
}

const FENCE_RE = /^---[ \t]*\r?\n([\s\S]*?)\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/

export function splitFrontmatter(md: string): SplitFrontmatter {
  const src = md.startsWith('﻿') ? md.slice(1) : md
  const m = src.match(FENCE_RE)
  if (!m) return { frontmatter: null, body: md }
  return { frontmatter: m[1], body: src.slice(m[0].length) }
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

function unquote(v: string): string {
  const t = v.trim()
  if (t.length >= 2 && ((t[0] === '"' && t.endsWith('"')) || (t[0] === "'" && t.endsWith("'")))) return t.slice(1, -1)
  return t
}

/**
 * HTML for a frontmatter block: a key/value table when every line is a flat
 * `key: value` pair, otherwise the YAML verbatim in a code block (nested maps,
 * lists and multi-line scalars don't fit a two-column table).
 */
export function frontmatterHtml(yaml: string): string {
  const rows: [string, string][] = []
  for (const line of yaml.split(/\r?\n/)) {
    if (!line.trim() || /^\s*#/.test(line)) continue
    const m = line.match(/^([A-Za-z0-9_.-]+):(?:\s+(.*))?$/)
    const value = m?.[2]?.trim() ?? ''
    if (!m || !value || /^[|>[{]/.test(value)) {
      return `<pre class="frontmatter"><code class="language-yaml">${escapeHtml(yaml)}</code></pre>`
    }
    rows.push([m[1], unquote(value)])
  }
  if (!rows.length) return ''
  const body = rows.map(([k, v]) => `<tr><th>${escapeHtml(k)}</th><td>${escapeHtml(v)}</td></tr>`).join('')
  return `<table class="frontmatter"><tbody>${body}</tbody></table>`
}
