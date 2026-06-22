import { estimateCost } from './pricing'

/** A pasted/attached image carried on a user message. `data` is raw base64. */
export interface ImageAttachment {
  media_type: string
  data: string
}

/**
 * A piece of editor context the IDE integration auto-injects into a user turn
 * (e.g. the file currently open, or the user's selection). It arrives wrapped in
 * an `<ide_*>` tag inside the user message text — not something the user typed —
 * so we lift it out into its own compact entry instead of rendering raw XML.
 */
export interface IdeContextItem {
  kind: 'opened' | 'selection' | 'other'
  /** Absolute file path the context refers to, when present. */
  path?: string
  /** Extra detail, e.g. a line range for a selection. */
  detail?: string
}

// Matches a complete `<ide_xxx>…</ide_xxx>` block (the IDE integration's
// context wrapper). Used to lift these out of the visible user text.
const IDE_TAG_RE = /<(ide_[a-z_]+)>([\s\S]*?)<\/\1>/g

function interpretIdeTag(tag: string, inner: string): IdeContextItem {
  const body = inner.trim()
  if (tag === 'ide_opened_file') {
    const m = body.match(/opened the file (.+?) in the IDE/i)
    return { kind: 'opened', path: m?.[1]?.trim() }
  }
  if (tag === 'ide_selection') {
    const range = body.match(/lines?\s+(\d+)\s+to\s+(\d+)\s+from\s+(.+?)\s*[:.]/i)
    if (range) return { kind: 'selection', path: range[3].trim(), detail: `L${range[1]}–${range[2]}` }
    const from = body.match(/from\s+(.+?)\s*[:.]/i)
    return { kind: 'selection', path: from?.[1]?.trim() }
  }
  return { kind: 'other', detail: body.slice(0, 80) }
}

/** Split IDE context tags out of a user message: returns the lifted items plus
 * the remaining text the user actually typed. */
export function parseIdeContext(text: string): { items: IdeContextItem[]; rest: string } {
  if (!text.includes('<ide_')) return { items: [], rest: text }
  const items: IdeContextItem[] = []
  const rest = text
    .replace(IDE_TAG_RE, (_m, tag: string, inner: string) => {
      items.push(interpretIdeTag(tag, inner))
      return ''
    })
    .trim()
  return { items, rest }
}

/**
 * Push a user turn, lifting any IDE context tags out of the raw `<ide_*>` XML
 * so it never shows up verbatim. The editor sends opened-file/selection context
 * as its own synthetic user message right before the one the user types, so we
 * also absorb a standalone `ide-context` entry sitting immediately before this
 * turn — the chip then rides inside the user bubble for a seamless look. A
 * truly orphan context (no typed message follows) stays as its own entry.
 */
function finalizeUserEntry(
  state: Pick<StreamState, 'entries'>,
  text: string,
  images?: ImageAttachment[],
): void {
  const { items, rest } = parseIdeContext(text)
  const merged = [...items]
  // Pull in any immediately-preceding standalone IDE context (chronological
  // order preserved: earlier synthetic context first, then this turn's own).
  while (state.entries.length) {
    const last = state.entries[state.entries.length - 1]
    if (last.kind !== 'ide-context') break
    merged.unshift(...last.items)
    state.entries.pop()
  }
  if (rest || images?.length) {
    state.entries.push({
      kind: 'user',
      text: rest,
      ...(images?.length ? { images } : {}),
      ...(merged.length ? { ideContext: merged } : {}),
    })
  } else if (merged.length) {
    state.entries.push({ kind: 'ide-context', items: merged })
  }
}

export type StreamEntry =
  | { kind: 'system'; model?: string; tools?: string[]; cwd?: string; sessionId?: string; slashCommands?: string[] }
  | { kind: 'text'; text: string }
  | { kind: 'thinking'; text: string }
  | { kind: 'user'; text: string; images?: ImageAttachment[]; ideContext?: IdeContextItem[] }
  | { kind: 'ide-context'; items: IdeContextItem[] }
  | {
      kind: 'tool'
      id: string
      name: string
      input: unknown
      result?: { content: string; is_error: boolean }
    }
  | {
      kind: 'result'
      text: string
      cost_usd?: number
      cost_estimated?: boolean
      total_tokens?: number
      duration_ms?: number
      num_turns?: number
      is_error: boolean
    }
  | { kind: 'error'; text: string }
  | { kind: 'log'; level: string; text: string }

export interface StreamState {
  entries: StreamEntry[]
  toolIndex: Map<string, number>
  /** Context-window occupancy after the last turn (parsed from a saved log). */
  contextTokens: number
  /** Model id seen on the latest `system.init` (for the context-window limit). */
  model: string | null
}

export function createStreamState(): StreamState {
  return { entries: [], toolIndex: new Map(), contextTokens: 0, model: null }
}

function blockText(content: unknown): string {
  if (typeof content === 'string') return content
  if (Array.isArray(content)) {
    return content
      .map((c) => {
        if (c && typeof c === 'object' && 'text' in c) return String((c as { text: unknown }).text ?? '')
        return JSON.stringify(c)
      })
      .join('\n')
  }
  if (content == null) return ''
  return JSON.stringify(content)
}

/** Extract a base64 image from a Claude-shaped image block (as persisted in the
 * log) or a Codex-shaped `image_url` data URL. Returns null if unrecognized. */
function parseImageBlock(b: Record<string, unknown>): ImageAttachment | null {
  const source = b.source
  if (source && typeof source === 'object') {
    const s = source as Record<string, unknown>
    if (typeof s.data === 'string' && typeof s.media_type === 'string') {
      return { media_type: s.media_type, data: s.data }
    }
  }
  if (typeof b.image_url === 'string') {
    const m = /^data:([^;]+);base64,(.*)$/s.exec(b.image_url)
    if (m) return { media_type: m[1], data: m[2] }
  }
  return null
}

export function applyStreamEvent(
  state: Pick<StreamState, 'entries' | 'toolIndex'>,
  evt: unknown,
): void {
  if (!evt || typeof evt !== 'object') return
  const e = evt as Record<string, unknown>
  const type = e.type

  if (type === 'system' && e.subtype === 'init') {
    state.entries.push({
      kind: 'system',
      model: typeof e.model === 'string' ? e.model : undefined,
      tools: Array.isArray(e.tools) ? (e.tools as string[]) : undefined,
      cwd: typeof e.cwd === 'string' ? e.cwd : undefined,
      sessionId: typeof e.session_id === 'string' ? e.session_id : undefined,
      slashCommands: Array.isArray(e.slash_commands) ? (e.slash_commands as unknown[]).map(String) : undefined,
    })
    return
  }

  if (type === 'assistant') {
    const message = e.message as Record<string, unknown> | undefined
    const content = (message?.content ?? []) as unknown[]
    for (const block of content) {
      if (!block || typeof block !== 'object') continue
      const b = block as Record<string, unknown>
      if (b.type === 'text') {
        state.entries.push({ kind: 'text', text: String(b.text ?? '') })
      } else if (b.type === 'thinking') {
        state.entries.push({ kind: 'thinking', text: String(b.thinking ?? '') })
      } else if (b.type === 'tool_use') {
        const id = String(b.id ?? '')
        const idx = state.entries.length
        state.entries.push({
          kind: 'tool',
          id,
          name: String(b.name ?? 'tool'),
          input: b.input,
        })
        if (id) state.toolIndex.set(id, idx)
      }
    }
    return
  }

  if (type === 'user') {
    const message = e.message as Record<string, unknown> | undefined
    const rawContent = message?.content
    // Plain-string content represents a user message we wrote via stdin
    // (the original prompt or a follow-up). Surface it as a 'user' entry.
    if (typeof rawContent === 'string') {
      const text = rawContent.trim()
      if (text) finalizeUserEntry(state, text)
      return
    }
    const content = (rawContent ?? []) as unknown[]
    // A real user turn we sent may bundle text + image blocks; merge them into a
    // single 'user' bubble. tool_result blocks (the SDK's echo of tool outputs)
    // are folded back into their originating tool entry instead.
    let userText = ''
    const userImages: ImageAttachment[] = []
    for (const block of content) {
      if (!block || typeof block !== 'object') continue
      const b = block as Record<string, unknown>
      if (b.type === 'tool_result') {
        const useId = String(b.tool_use_id ?? '')
        const text = blockText(b.content)
        const isError = !!b.is_error
        const idx = state.toolIndex.get(useId)
        if (idx !== undefined) {
          const entry = state.entries[idx]
          if (entry && entry.kind === 'tool') {
            entry.result = { content: text, is_error: isError }
          }
        }
      } else if (b.type === 'text') {
        const text = String(b.text ?? '').trim()
        if (text) userText = userText ? `${userText}\n${text}` : text
      } else if (b.type === 'image') {
        const img = parseImageBlock(b)
        if (img) userImages.push(img)
      }
    }
    if (userText || userImages.length) {
      finalizeUserEntry(state, userText, userImages)
    }
    return
  }

  if (type === 'result') {
    // Claude's SDK echoes the final assistant message in `result`, which we
    // already rendered as a `text` entry — drop it here so the "Completed"
    // summary doesn't duplicate that content. (Codex sends no result text.)
    const resultText = String(e.result ?? '')
    const lastText = [...state.entries].reverse().find((x) => x.kind === 'text')
    const isDuplicate = !!resultText && lastText?.kind === 'text' && lastText.text.trim() === resultText.trim()

    // Token totals + cost. Both engines carry `usage`; Claude also reports
    // `total_cost_usd` directly, while Codex's dollar figure must be estimated
    // from the token counts and the model id seen on `system.init`.
    const usage = (e.usage ?? {}) as Record<string, unknown>
    const tok = (k: string) => (typeof usage[k] === 'number' ? (usage[k] as number) : 0)
    const inputTokens = tok('input_tokens')
    const outputTokens = tok('output_tokens')
    const cacheCreate = tok('cache_creation_input_tokens')
    const cacheRead = tok('cache_read_input_tokens')
    const totalTokens = inputTokens + outputTokens + cacheCreate + cacheRead

    const reportedCost = typeof e.total_cost_usd === 'number' ? e.total_cost_usd : undefined
    let costUsd = reportedCost
    let costEstimated = false
    if (costUsd === undefined && totalTokens > 0) {
      const model = [...state.entries].reverse().find((x) => x.kind === 'system')
      const modelId = model?.kind === 'system' ? model.model : undefined
      costUsd = estimateCost(modelId ?? '', inputTokens, outputTokens, cacheCreate, cacheRead)
      costEstimated = true
    }

    state.entries.push({
      kind: 'result',
      text: isDuplicate ? '' : resultText,
      cost_usd: costUsd,
      cost_estimated: costEstimated,
      total_tokens: totalTokens > 0 ? totalTokens : undefined,
      duration_ms: typeof e.duration_ms === 'number' ? e.duration_ms : undefined,
      num_turns: typeof e.num_turns === 'number' ? e.num_turns : undefined,
      is_error: !!e.is_error,
    })
    return
  }
}

/** Sum every input-side token (incl. cache) plus output for one `usage` blob. */
function sumUsage(usage: Record<string, unknown> | undefined): number | null {
  if (!usage || typeof usage !== 'object') return null
  const tok = (k: string) => (typeof usage[k] === 'number' ? (usage[k] as number) : 0)
  const total =
    tok('input_tokens') +
    tok('output_tokens') +
    tok('cache_creation_input_tokens') +
    tok('cache_read_input_tokens')
  return total > 0 ? total : null
}

/**
 * Tokens occupying the context window after a single `assistant` message — its
 * per-API-call `usage` (uncached input + cache read/creation + output). This is
 * the true window occupancy at that point in the turn: the whole prior history
 * rides in as `cache_read_input_tokens`, so the figure grows turn over turn.
 *
 * Deliberately ignores `result` events: in an agentic turn the SDK makes many
 * model calls and the `result` usage is their *cumulative sum*, which massively
 * over-counts the window (and swings with how many tool calls ran). Use
 * `extractTurnTotalTokens` for that cumulative figure instead.
 */
export function extractContextTokens(evt: unknown): number | null {
  if (!evt || typeof evt !== 'object') return null
  const e = evt as Record<string, unknown>
  if (e.type !== 'assistant') return null
  const message = e.message as Record<string, unknown> | undefined
  return sumUsage(message?.usage as Record<string, unknown> | undefined)
}

/**
 * Cumulative token usage for a whole turn, read from the `result` event. Sums
 * across every model call the turn made, so it is *not* a context-window size —
 * use it only as a last-resort occupancy estimate for engines that never emit
 * per-message `assistant` usage.
 */
export function extractTurnTotalTokens(evt: unknown): number | null {
  if (!evt || typeof evt !== 'object') return null
  const e = evt as Record<string, unknown>
  if (e.type !== 'result') return null
  return sumUsage(e.usage as Record<string, unknown> | undefined)
}

/** True when the event is a compaction boundary (context was just shrunk). */
export function isCompactBoundary(evt: unknown): boolean {
  if (!evt || typeof evt !== 'object') return false
  const e = evt as Record<string, unknown>
  return e.type === 'system' && e.subtype === 'compact_boundary'
}

export function parseStreamLog(raw: string): StreamState | null {
  const lines = raw.split(/\r?\n/).filter((l) => l.trim().length > 0)
  if (lines.length === 0) return null

  let firstNonStderr: string | null = null
  for (const line of lines) {
    if (line.startsWith('[stderr]')) continue
    firstNonStderr = line
    break
  }
  if (!firstNonStderr) return null

  try {
    const head = JSON.parse(firstNonStderr)
    if (!head || typeof head !== 'object' || typeof (head as Record<string, unknown>).type !== 'string') {
      return null
    }
  } catch {
    return null
  }

  const state = createStreamState()
  // Reconstruct context-window occupancy the same way the live store does:
  // high-water mark of per-message `assistant` usage, reset on compaction, with
  // the cumulative `result` total only as a fallback for engines that never
  // emit per-message usage. See `extractContextTokens`.
  let sawAssistantUsage = false
  for (const line of lines) {
    const logMatch = line.match(/^\[log:([a-z]+)\]\s?([\s\S]*)$/)
    if (logMatch) {
      state.entries.push({ kind: 'log', level: logMatch[1], text: logMatch[2] })
      continue
    }
    if (line.startsWith('[stderr]')) {
      state.entries.push({ kind: 'error', text: line.slice('[stderr]'.length).trim() })
      continue
    }
    try {
      const parsed = JSON.parse(line)
      applyStreamEvent(state, parsed)
      if (isCompactBoundary(parsed)) {
        state.contextTokens = 0
        sawAssistantUsage = false
      } else {
        const ctx = extractContextTokens(parsed)
        if (ctx !== null) {
          state.contextTokens = Math.max(state.contextTokens, ctx)
          sawAssistantUsage = true
        } else if (!sawAssistantUsage) {
          const total = extractTurnTotalTokens(parsed)
          if (total !== null) state.contextTokens = total
        }
      }
    } catch {
    }
  }
  // Latest model id seen on a system.init entry — feeds the context limit.
  for (let i = state.entries.length - 1; i >= 0; i--) {
    const e = state.entries[i]
    if (e.kind === 'system' && e.model) {
      state.model = e.model
      break
    }
  }
  return state
}

function ideContextLine(it: IdeContextItem): string {
  const verb = it.kind === 'selection' ? 'Selected' : it.kind === 'opened' ? 'Opened' : 'Context'
  return `⊕ ${verb}${it.path ? ` ${it.path}` : ''}${it.detail ? ` ${it.detail}` : ''}`.trimEnd()
}

export function entriesToPlainText(entries: StreamEntry[]): string {
  const parts: string[] = []
  for (const e of entries) {
    switch (e.kind) {
      case 'system':
        parts.push(`▸ session started${e.model ? ` (${e.model})` : ''}`)
        break
      case 'text':
        parts.push(e.text)
        break
      case 'user': {
        const ctx = (e.ideContext ?? []).map(ideContextLine)
        parts.push([...ctx, `> ${e.text}`].join('\n'))
        break
      }
      case 'ide-context':
        for (const it of e.items) parts.push(ideContextLine(it))
        break
      case 'thinking':
        parts.push(`(thinking) ${e.text}`)
        break
      case 'tool': {
        const inputStr =
          typeof e.input === 'string' ? e.input : JSON.stringify(e.input ?? {})
        parts.push(`◆ ${e.name}(${inputStr.length > 200 ? inputStr.slice(0, 200) + '…' : inputStr})`)
        if (e.result) {
          const head = e.result.content.split('\n').slice(0, 6).join('\n')
          parts.push(`  ${e.result.is_error ? '✗' : '→'} ${head}${
            e.result.content.split('\n').length > 6 ? '\n  …' : ''
          }`)
        }
        break
      }
      case 'result': {
        const dur = e.duration_ms ? `${(e.duration_ms / 1000).toFixed(1)}s` : ''
        const cost = e.cost_usd ? `$${e.cost_usd.toFixed(4)}` : ''
        const turns = e.num_turns ? `${e.num_turns} turns` : ''
        const meta = [dur, turns, cost].filter(Boolean).join(' · ')
        parts.push(`${e.is_error ? '✗ failed' : '✓ done'}${meta ? ' — ' + meta : ''}`)
        if (e.text) parts.push(e.text)
        break
      }
      case 'error':
        parts.push(`[stderr] ${e.text}`)
        break
      case 'log':
        parts.push(`[${e.level}] ${e.text}`)
        break
    }
  }
  return parts.join('\n\n')
}
