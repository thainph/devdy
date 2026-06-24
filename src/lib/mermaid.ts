// Mermaid diagram support for the markdown viewer.
//
// Two pieces work together:
//   1. `applyMermaidFence(md)` — teaches a markdown-it instance to emit a
//      placeholder `<div class="mermaid-diagram">` (carrying the raw source in
//      a data attribute, plus a `<pre>` fallback) instead of a normal code
//      block for ```mermaid fences.
//   2. `vMermaid` — a Vue directive applied to the element that holds the
//      rendered markdown. After each v-html update it scans for placeholders
//      and swaps in the rendered SVG.
//
// Kept framework-agnostic on the render side so the same logic serves the
// stream view (StreamLog), RunView's legacy columns, and the rule/skill
// editor previews — all of which render markdown via v-html.

import type { Directive } from 'vue'
import type MarkdownIt from 'markdown-it'
import { enableMermaidZoom } from './mermaidZoom'

/** Register a custom fence renderer so ```mermaid blocks become diagram placeholders. */
export function applyMermaidFence(md: MarkdownIt): void {
  const fallback = md.renderer.rules.fence
  md.renderer.rules.fence = (tokens, idx, options, env, self) => {
    const token = tokens[idx]
    const lang = (token.info || '').trim().split(/\s+/)[0].toLowerCase()
    if (lang === 'mermaid') {
      const src = token.content
      // encodeURIComponent keeps quotes/newlines/unicode safe inside an attribute.
      const enc = encodeURIComponent(src)
      // The inner `.mermaid-render` is what the directive swaps for the SVG; the
      // `<pre>` is the pre-render / fallback view. Keeping the SVG in an inner
      // wrapper means a copy button appended to the outer `.mermaid-diagram`
      // survives re-renders (see vCopyCode).
      return (
        `<div class="mermaid-diagram" data-mermaid="${enc}">` +
        `<div class="mermaid-render"><pre class="mermaid-src">${md.utils.escapeHtml(src)}</pre></div>` +
        `</div>`
      )
    }
    // Not mermaid: defer to markdown-it's default fence renderer.
    if (fallback) return fallback(tokens, idx, options, env, self)
    return self.renderToken(tokens, idx, options)
  }
}

// --- SVG rendering -----------------------------------------------------------

let mermaidPromise: Promise<typeof import('mermaid').default> | null = null
let lastTheme: 'dark' | 'default' | null = null
let idCounter = 0
// Cache rendered SVG by `${theme}\n${source}` so re-renders (frequent during
// streaming) don't re-run the layout engine or flicker.
const svgCache = new Map<string, string>()

function currentTheme(): 'dark' | 'default' {
  return document.documentElement.classList.contains('dark') ? 'dark' : 'default'
}

async function loadMermaid() {
  if (!mermaidPromise) {
    mermaidPromise = import('mermaid').then((m) => m.default)
  }
  const mermaid = await mermaidPromise
  const theme = currentTheme()
  if (theme !== lastTheme) {
    mermaid.initialize({
      startOnLoad: false,
      theme,
      securityLevel: 'strict',
      fontFamily: 'inherit',
    })
    lastTheme = theme
  }
  return mermaid
}

/** Render every un-rendered `.mermaid-diagram` placeholder inside `el`. */
export async function renderMermaidIn(el: HTMLElement): Promise<void> {
  const blocks = Array.from(el.querySelectorAll<HTMLElement>('.mermaid-diagram'))
  if (!blocks.length) return

  let mermaid: Awaited<ReturnType<typeof loadMermaid>>
  try {
    mermaid = await loadMermaid()
  } catch {
    return // mermaid failed to load; leave the raw source fallback in place
  }
  const theme = currentTheme()

  for (const block of blocks) {
    // Render into the inner wrapper so sibling decorations (e.g. a copy button
    // on the outer block) aren't wiped. Fall back to the block itself for safety.
    const target = block.querySelector<HTMLElement>('.mermaid-render') ?? block
    const src = decodeURIComponent(block.dataset.mermaid ?? '').trim()
    if (!src) continue
    const cacheKey = `${theme}\n${src}`
    // Already showing this exact diagram for this theme — nothing to do.
    if (block.dataset.rendered === cacheKey) continue

    const cached = svgCache.get(cacheKey)
    if (cached) {
      target.innerHTML = cached
      block.dataset.rendered = cacheKey
      enableMermaidZoom(block)
      continue
    }

    // During streaming the source may be incomplete; only render once it parses
    // cleanly, otherwise keep the raw-source fallback and retry on the next update.
    let valid = false
    try {
      valid = !!(await mermaid.parse(src, { suppressErrors: true }))
    } catch {
      valid = false
    }
    if (!valid) continue

    try {
      const { svg } = await mermaid.render(`mermaid-${idCounter++}`, src)
      svgCache.set(cacheKey, svg)
      target.innerHTML = svg
      block.dataset.rendered = cacheKey
      enableMermaidZoom(block)
    } catch {
      // Unexpected render failure: leave the raw-source fallback visible.
    }
  }
}

/**
 * Directive for the markdown host element. Re-renders diagrams whenever the
 * v-html content mounts or updates. Fire-and-forget (async) so it never blocks
 * Vue's patch cycle.
 */
export const vMermaid: Directive<HTMLElement> = {
  mounted: (el) => {
    void renderMermaidIn(el)
  },
  updated: (el) => {
    void renderMermaidIn(el)
  },
}
