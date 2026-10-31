// Pure helpers behind the MCP servers screen: the recommended catalog, the
// sidebar rows (user servers, the built-in native servers, catalog entries),
// their filtering + grouping, and the editor form's parsing / validation /
// dirty snapshot. Secret VALUEs only ever pass through in memory here — never
// into a row, a URL or a log.
import { haystack, matchesSearch, type RowGroup } from './listGroups.ts'

export type McpTransport = 'stdio' | 'http' | 'sse'

/** The subset of the store's McpServer these helpers read. */
export interface McpServerLike {
  id: string
  name: string
  description: string
  transport: McpTransport
  command: string | null
  args: string[]
  url: string | null
  enabled: boolean
}

/**
 * One-click catalog of commonly useful third-party MCP servers. Adding one
 * creates a stub (empty secret values) the user finishes in the detail pane.
 */
export interface CatalogEntry {
  name: string
  description: string
  transport: McpTransport
  command?: string
  args?: string[]
  url?: string
  env?: string[]
}

export const MCP_CATALOG: CatalogEntry[] = [
  { name: 'sequential-thinking', description: 'Structured step-by-step reasoning scratchpad.', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-sequential-thinking'] },
  { name: 'context7', description: 'Up-to-date library docs to reduce hallucinated APIs.', transport: 'http', url: 'https://mcp.context7.com/mcp' },
  { name: 'slack', description: 'Read Slack channels & threads (user token, xoxp-).', transport: 'stdio', command: 'npx', args: ['-y', 'slack-mcp-server@latest', '--transport', 'stdio'], env: ['SLACK_MCP_XOXP_TOKEN'] },
  { name: 'notion', description: 'Search & edit Notion pages and databases (integration secret).', transport: 'stdio', command: 'npx', args: ['-y', '@notionhq/notion-mcp-server'], env: ['NOTION_TOKEN'] },
  { name: 'atlassian', description: 'Jira issues & Confluence pages. Requires uv (uvx).', transport: 'stdio', command: 'uvx', args: ['mcp-atlassian'], env: ['JIRA_URL', 'JIRA_USERNAME', 'JIRA_API_TOKEN', 'CONFLUENCE_URL', 'CONFLUENCE_USERNAME', 'CONFLUENCE_API_TOKEN'] },
  { name: 'figma', description: 'Design context from Figma Desktop (enable the MCP server in Dev Mode).', transport: 'http', url: 'http://127.0.0.1:3845/mcp' },
  { name: 'chrome-devtools', description: 'Inspect console, network & performance in a live Chrome.', transport: 'stdio', command: 'npx', args: ['-y', 'chrome-devtools-mcp@latest'] },
  { name: 'playwright', description: 'Drive a real browser for testing & scraping.', transport: 'stdio', command: 'npx', args: ['-y', '@playwright/mcp@latest'] },
]

/** Catalog entries whose name is not taken yet (server names are unique). */
export function availableCatalog(
  catalog: readonly CatalogEntry[],
  servers: readonly Pick<McpServerLike, 'name'>[],
): CatalogEntry[] {
  const taken = new Set(servers.map((s) => s.name))
  return catalog.filter((c) => !taken.has(c.name))
}

// ── Sidebar rows ────────────────────────────────────────────────────────────

/** Row ids of the non-server rows; they never collide with server UUIDs. */
export const BUILTIN_PREFIX = 'builtin:'
export const CATALOG_PREFIX = 'catalog:'

export type McpRowKind = 'server' | 'builtin' | 'catalog'
export type BuiltinTone = 'success' | 'neutral' | 'warning'

export interface McpRow {
  id: string
  kind: McpRowKind
  name: string
  /** The one-line command (stdio) or URL (http/sse); built-ins: their tool glob. */
  line: string
  description: string
  transport: McpTransport | null
  /** Built-ins: injected right now; catalog: false (not added). */
  enabled: boolean
  /** Built-ins only. */
  tone?: BuiltinTone
}

/** What a built-in native server looks like to the screen. */
export interface BuiltinInfo {
  name: string
  tools: string
  tone: BuiltinTone
}

/** `command arg…` for stdio, the URL for http/sse. */
export function serverLine(s: Pick<McpServerLike, 'transport' | 'command' | 'args' | 'url'>): string {
  if (s.transport === 'stdio') return [s.command ?? '', ...(s.args ?? [])].filter(Boolean).join(' ')
  return s.url ?? ''
}

export function serverRow(s: McpServerLike): McpRow {
  return {
    id: s.id,
    kind: 'server',
    name: s.name,
    line: serverLine(s),
    description: s.description,
    transport: s.transport,
    enabled: s.enabled,
  }
}

export function builtinRow(b: BuiltinInfo, description: string): McpRow {
  return {
    id: BUILTIN_PREFIX + b.name,
    kind: 'builtin',
    name: b.name,
    line: b.tools,
    description,
    transport: null,
    enabled: b.tone === 'success',
    tone: b.tone,
  }
}

export function catalogRow(c: CatalogEntry): McpRow {
  return {
    id: CATALOG_PREFIX + c.name,
    kind: 'catalog',
    name: c.name,
    line: serverLine({ transport: c.transport, command: c.command ?? null, args: c.args ?? [], url: c.url ?? null }),
    description: c.description,
    transport: c.transport,
    enabled: false,
  }
}

/** '' = all, else by the enabled state (catalog entries only show under all). */
export type McpFilter = '' | 'enabled' | 'disabled'

export function matchesMcpRow(row: McpRow, search: string, filter: McpFilter): boolean {
  if (filter && row.kind === 'catalog') return false
  if (filter === 'enabled' && !row.enabled) return false
  if (filter === 'disabled' && row.enabled) return false
  return matchesSearch(haystack(row.name, row.line, row.description), search)
}

export const GROUP_BUILTIN = 'builtin'
export const GROUP_CATALOG = 'catalog'
const TRANSPORT_ORDER: McpTransport[] = ['stdio', 'http', 'sse']

export interface McpGroupLabels {
  builtin: string
  catalog: string
  transport: (t: McpTransport) => string
}

/**
 * Sidebar folders: Built-in first, then the user's servers by transport
 * (stdio, http, sse — a fixed order, not A–Z), Recommended last. Empty
 * folders are dropped; rows keep their input order.
 */
export function groupMcpRows(rows: readonly McpRow[], labels: McpGroupLabels): RowGroup<McpRow>[] {
  const out: RowGroup<McpRow>[] = []
  const push = (key: string, label: string, list: McpRow[]) => {
    if (list.length > 0) out.push({ key, label, rows: list })
  }
  push(GROUP_BUILTIN, labels.builtin, rows.filter((r) => r.kind === 'builtin'))
  for (const t of TRANSPORT_ORDER) {
    push(t, labels.transport(t), rows.filter((r) => r.kind === 'server' && r.transport === t))
  }
  push(GROUP_CATALOG, labels.catalog, rows.filter((r) => r.kind === 'catalog'))
  return out
}

// ── Editor form ─────────────────────────────────────────────────────────────

/**
 * One editable secret row. `existing` marks a key that already has a stored
 * VALUE (never surfaced to the UI); an empty `value` on such a row = keep it.
 */
export interface SecretRow {
  key: string
  value: string
  existing: boolean
}

/** Mirrors the store's SecretEntry: omitted `value` = keep the stored one. */
export interface SecretEntryLike {
  key: string
  value?: string
}

export interface McpForm {
  name: string
  description: string
  transport: McpTransport
  command: string
  argsText: string
  url: string
  envRows: SecretRow[]
  headerRows: SecretRow[]
  enabled: boolean
}

export function blankForm(): McpForm {
  return {
    name: '',
    description: '',
    transport: 'stdio',
    command: '',
    argsText: '',
    url: '',
    envRows: [],
    headerRows: [],
    enabled: true,
  }
}

/** A saved server as a form; its secret rows are key-only (`existing`). */
export function formFromServer(
  s: McpServerLike & { env_keys: string[]; header_keys: string[] },
): McpForm {
  return {
    name: s.name,
    description: s.description,
    transport: s.transport,
    command: s.command ?? '',
    argsText: (s.args ?? []).join('\n'),
    url: s.url ?? '',
    envRows: s.env_keys.map((key) => ({ key, value: '', existing: true })),
    headerRows: s.header_keys.map((key) => ({ key, value: '', existing: true })),
    enabled: s.enabled,
  }
}

/**
 * A new draft copied from a saved server. Stored secret VALUEs can't be read
 * back, so the keys come over with empty values for the user to fill in.
 */
export function duplicateForm(form: McpForm, name: string): McpForm {
  const fresh = (rows: SecretRow[]) => rows.map((r) => ({ key: r.key, value: '', existing: false }))
  return { ...form, name, envRows: fresh(form.envRows), headerRows: fresh(form.headerRows) }
}

/** `<name>-copy`, `<name>-copy-2`, … — the first one not taken. */
export function copyName(name: string, taken: readonly string[]): string {
  const used = new Set(taken)
  let candidate = `${name}-copy`
  for (let i = 2; used.has(candidate); i++) candidate = `${name}-copy-${i}`
  return candidate
}

export function isRemote(transport: McpTransport): boolean {
  return transport === 'http' || transport === 'sse'
}

/** One arg per non-blank line, trimmed. */
export function parseArgs(text: string): string[] {
  return text
    .split('\n')
    .map((a) => a.trim())
    .filter((a) => a.length > 0)
}

/** Backend secret entries: an existing key left blank omits `value` (keep stored). */
export function toSecretEntries(rows: readonly SecretRow[]): SecretEntryLike[] {
  return rows
    .filter((r) => r.key.trim().length > 0)
    .map((r) => (r.existing && r.value === '' ? { key: r.key.trim() } : { key: r.key.trim(), value: r.value }))
}

/** The create/update payload (minus `id`): only the transport-relevant fields. */
export function formPayload(form: McpForm) {
  const stdio = form.transport === 'stdio'
  const remote = isRemote(form.transport)
  return {
    name: form.name.trim(),
    description: form.description,
    transport: form.transport,
    command: stdio ? form.command.trim() : null,
    args: stdio ? parseArgs(form.argsText) : [],
    url: remote ? form.url.trim() : null,
    env: stdio ? toSecretEntries(form.envRows) : [],
    headers: remote ? toSecretEntries(form.headerRows) : [],
    enabled: form.enabled,
  }
}

/** An i18n key under `mcp.editor.validation` + its params. */
export interface FormError {
  key: string
  params?: Record<string, string>
}

/** Just the command / URL a connection test needs. */
export function validateTarget(form: Pick<McpForm, 'transport' | 'command' | 'url'>): FormError | null {
  if (form.transport === 'stdio') {
    return form.command.trim() ? null : { key: 'commandRequired' }
  }
  return form.url.trim() ? null : { key: 'urlRequired', params: { transport: form.transport } }
}

export function validateForm(form: McpForm): FormError | null {
  const name = form.name.trim()
  if (!name) return { key: 'nameRequired' }
  if (!/^[a-zA-Z0-9_-]+$/.test(name)) return { key: 'nameFormat' }
  const target = validateTarget(form)
  if (target) return target
  const rows = isRemote(form.transport) ? form.headerRows : form.envRows
  const seen = new Set<string>()
  for (const r of rows) {
    const k = r.key.trim()
    if (!k) return { key: 'keyEmpty' }
    if (seen.has(k)) return { key: 'duplicateKey', params: { key: k } }
    seen.add(k)
  }
  return null
}

/**
 * Comparable snapshot for the dirty check (kept in memory only). `withEnabled`
 * false leaves the switch out: a saved server's enabled state applies at once.
 */
export function formSnapshot(form: McpForm, withEnabled: boolean): string {
  return JSON.stringify({ ...form, enabled: withEnabled ? form.enabled : null })
}
