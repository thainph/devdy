// Slack mrkdwn → HTML / plain text, the pure half of lib/slackMrkdwn.ts (which
// adds the DOMPurify pass). Both the original (`textRaw`) and the Vietnamese
// (`textVi`) of a message are Slack mrkdwn, so one renderer serves both.
//
// Slack escapes `<`, `>` and `&` in message text, so every bare `<…>` is a
// Slack token (mention, channel, special mention, link). Rendering goes:
//   1. cut code blocks / inline code / `<…>` tokens out into placeholders (their
//      content is never formatted);
//   2. decode the three entities and HTML-escape what is left;
//   3. emoji shortcodes, then *bold* _italic_ ~strike~ on word boundaries;
//   4. `>` lines become blockquotes; everything else keeps its line breaks
//      (the host renders with `white-space: pre-wrap`, so lists stay as typed);
//   5. placeholders go back in.
// The output only contains markup this module writes with escaped content, and
// is still sanitized by the caller.
//
// Pure module (relative imports only): unit-tested with `node --test`.
import { emojiFor } from './slackEmoji.ts'

export interface MrkdwnUser {
  displayName: string
  realName: string | null
}

export interface MrkdwnContext {
  users: Record<string, MrkdwnUser>
  /** channel_id → name (without '#'). */
  channels: Record<string, string>
  /** e.g. malme.slack.com, for `#channel` links; null = not clickable. */
  workspaceDomain: string | null
}

// Placeholder delimiters from the Private Use Area: never in Slack text.
const PH_OPEN = ''
const PH_CLOSE = ''
const PH_RE = /(\d+)/g

const MENTION_CLASS = 'slack-mention rounded px-0.5 bg-primary/10 text-primary font-medium'
const LINK_CLASS = 'slack-link text-primary underline-offset-2 hover:underline cursor-pointer'

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

/** Slack only ever escapes these three. */
export function decodeEntities(s: string): string {
  return s.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&')
}

const SAFE_URL = /^(https?:\/\/|mailto:)/i

/** What a `<…>` token stands for (shared by the HTML and the plain renderers). */
type Token =
  | { kind: 'user'; id: string; label: string; title: string | null }
  | { kind: 'channel'; id: string; label: string; href: string | null }
  | { kind: 'special'; label: string }
  | { kind: 'link'; href: string; label: string }
  | { kind: 'text'; text: string }

function resolveToken(inner: string, ctx: MrkdwnContext): Token {
  const bar = inner.indexOf('|')
  const target = bar === -1 ? inner : inner.slice(0, bar)
  const label = bar === -1 ? '' : decodeEntities(inner.slice(bar + 1))

  if (/^@[UW][A-Z0-9]+$/.test(target)) {
    const id = target.slice(1)
    const user = ctx.users[id]
    return {
      kind: 'user',
      id,
      label: user?.displayName || label.replace(/^@/, '') || id,
      title: user?.realName ?? null,
    }
  }
  if (/^#[CG][A-Z0-9]+$/.test(target)) {
    const id = target.slice(1)
    const name = label || ctx.channels[id] || id
    return {
      kind: 'channel',
      id,
      label: name,
      href: ctx.workspaceDomain ? `https://${ctx.workspaceDomain}/archives/${id}` : null,
    }
  }
  if (target.startsWith('!')) {
    const word = target.slice(1)
    if (word === 'here' || word === 'channel' || word === 'everyone') {
      return { kind: 'special', label: `@${word}` }
    }
    if (word.startsWith('subteam^')) {
      return { kind: 'special', label: label || '@group' }
    }
    // <!date^…|fallback> and anything newer: the fallback is the readable part.
    return label ? { kind: 'text', text: label } : { kind: 'special', label: `@${word.split('^')[0]}` }
  }
  const href = decodeEntities(target)
  if (SAFE_URL.test(href)) {
    return { kind: 'link', href, label: label || href.replace(/^mailto:/i, '') }
  }
  return { kind: 'text', text: `<${decodeEntities(inner)}>` }
}

function tokenHtml(tok: Token): string {
  switch (tok.kind) {
    case 'user':
      return `<span class="${MENTION_CLASS}"${tok.title ? ` title="${escapeHtml(tok.title)}"` : ''}>@${escapeHtml(tok.label)}</span>`
    case 'channel':
      return tok.href
        ? `<a class="${LINK_CLASS}" href="${escapeHtml(tok.href)}">#${escapeHtml(tok.label)}</a>`
        : `<span class="${MENTION_CLASS}">#${escapeHtml(tok.label)}</span>`
    case 'special':
      return `<span class="${MENTION_CLASS}">${escapeHtml(tok.label)}</span>`
    case 'link':
      return `<a class="${LINK_CLASS}" href="${escapeHtml(tok.href)}">${escapeHtml(tok.label)}</a>`
    case 'text':
      return escapeHtml(tok.text)
  }
}

function tokenPlain(tok: Token): string {
  switch (tok.kind) {
    case 'user':
      return `@${tok.label}`
    case 'channel':
      return `#${tok.label}`
    case 'special':
      return tok.label
    case 'link': {
      const bare = tok.href.replace(/^mailto:/i, '')
      return tok.label === tok.href || tok.label === bare ? tok.label : `${tok.label} (${tok.href})`
    }
    case 'text':
      return tok.text
  }
}

/** `:name:` (and `:name::skin-tone-N:`) → unicode; unknown names stay as typed. */
export function replaceEmoji(text: string): string {
  return text.replace(/:([a-z0-9_+\-']+(?:::skin-tone-[2-6])?):/g, (whole, name: string) => {
    return emojiFor(name) ?? whole
  })
}

// Slack-style word boundaries: the opening marker follows a line start / space /
// opening punctuation and is followed by a non-space; the closing marker follows
// a non-space and precedes a line end / space / punctuation. Never across lines.
// The text is already HTML-escaped here, so a quote / `<` `>` `&` next to a
// marker reads as its entity (`"*q*"` → `&quot;*q*&quot;`).
const ENTITY = '&(?:quot|amp|lt|gt);'
const OPEN_BEFORE = `(^|[\\s(\\[{"'“‘${PH_CLOSE}]|${ENTITY})`
const CLOSE_AFTER = `(?=$|[\\s.,!?;:)\\]}"'”’\\-${PH_OPEN}]|${ENTITY})`

function emphasis(marker: string, tag: string): [RegExp, string] {
  const m = `\\${marker}`
  return [
    new RegExp(`${OPEN_BEFORE}${m}(?=[^\\s${m}])([^${m}\\n]*?[^\\s${m}])${m}${CLOSE_AFTER}`, 'gm'),
    `$1<${tag}>$2</${tag}>`,
  ]
}

const EMPHASIS: [RegExp, string][] = [
  emphasis('*', 'strong'),
  emphasis('_', 'em'),
  emphasis('~', 'del'),
]

function applyEmphasis(html: string): string {
  let out = html
  for (const [re, rep] of EMPHASIS) out = out.replace(re, rep)
  return out
}

interface Cut {
  text: string
  slots: string[]
  /** Slot indexes that render as blocks (code blocks). */
  blocks: Set<number>
}

/** Step 1: code blocks, inline code and `<…>` tokens → placeholders. */
function cutOut(text: string, render: (inner: string) => string, code: {
  block: (body: string) => string
  inline: (body: string) => string
}): Cut {
  const slots: string[] = []
  const blocks = new Set<number>()
  const put = (html: string, block = false) => {
    slots.push(html)
    if (block) blocks.add(slots.length - 1)
    return `${PH_OPEN}${slots.length - 1}${PH_CLOSE}`
  }
  let out = text.replace(/```([\s\S]*?)```/g, (_, body: string) => put(code.block(body), true))
  out = out.replace(/`([^`\n]+)`/g, (_, body: string) => put(code.inline(body)))
  out = out.replace(/<([^<>\n]+)>/g, (_, inner: string) => put(render(inner)))
  return { text: out, slots, blocks }
}

function restore(text: string, slots: string[]): string {
  return text.replace(PH_RE, (_, i: string) => slots[Number(i)] ?? '')
}

/**
 * Slack mrkdwn → HTML (NOT sanitized; use renderSlackMrkdwn from
 * lib/slackMrkdwn.ts in the UI). Line breaks are kept as `\n`: render inside a
 * `white-space: pre-wrap` container.
 */
export function mrkdwnToHtml(text: string, ctx: MrkdwnContext): string {
  if (!text) return ''
  const cut = cutOut(text, (inner) => tokenHtml(resolveToken(inner, ctx)), {
    block: (body) =>
      `<pre class="slack-code-block my-1 rounded-md border border-border/60 bg-muted/50 px-2.5 py-2 font-mono text-[12px] whitespace-pre-wrap break-words"><code>${escapeHtml(decodeEntities(body.replace(/^\n/, '').replace(/\n$/, '')))}</code></pre>`,
    inline: (body) =>
      `<code class="slack-code rounded border border-border/60 bg-muted/60 px-1 py-px font-mono text-[12px] text-destructive">${escapeHtml(decodeEntities(body))}</code>`,
  })

  // Steps 2–3, line by line, so quotes can be grouped afterwards.
  const lines = cut.text.split('\n').map((line) => {
    const plain = decodeEntities(line)
    const quote = /^>\s?/.exec(plain)
    const body = quote ? plain.slice(quote[0].length) : plain
    return { quote: !!quote, html: applyEmphasis(replaceEmoji(escapeHtml(body))) }
  })

  // Step 4: consecutive `>` lines → one blockquote; blocks eat their own newline.
  const parts: string[] = []
  let buffer: string[] = []
  const flush = () => {
    if (buffer.length) parts.push(buffer.join('\n'))
    buffer = []
  }
  for (let i = 0; i < lines.length; ) {
    if (lines[i].quote) {
      flush()
      const quoted: string[] = []
      while (i < lines.length && lines[i].quote) quoted.push(lines[i++].html)
      parts.push(
        `<blockquote class="slack-quote my-0.5 border-l-4 border-border pl-2.5 text-muted-foreground">${quoted.join('\n')}</blockquote>`,
      )
    } else {
      buffer.push(lines[i++].html)
    }
  }
  flush()

  let html = parts.join('')
  // A code block is a block element: drop the newline right before / after it so
  // pre-wrap doesn't add an empty line on each side.
  html = html.replace(/\n?((\d+))\n?/g, (whole, ph: string, i: string) =>
    cut.blocks.has(Number(i)) ? ph : whole,
  )
  return restore(html, cut.slots)
}

/**
 * Slack mrkdwn → plain text for the clipboard: mentions → `@Name`, channels →
 * `#name`, links → `label (url)`, entities decoded, emoji as unicode; code and
 * formatting markers are left as typed.
 */
export function mrkdwnToPlain(text: string, ctx: MrkdwnContext): string {
  if (!text) return ''
  const cut = cutOut(text, (inner) => tokenPlain(resolveToken(inner, ctx)), {
    block: (body) => '```' + decodeEntities(body) + '```',
    inline: (body) => '`' + decodeEntities(body) + '`',
  })
  return restore(replaceEmoji(decodeEntities(cut.text)), cut.slots)
}
