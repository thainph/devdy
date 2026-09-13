// Shared markdown renderer used by the run view and the pop-out permission
// window, so both render ExitPlanMode plans / streamed output identically
// without each duplicating the markdown-it setup.
//
// `renderText` is called from inside v-for templates (StreamLog), so Vue
// re-invokes it for EVERY entry on every re-render. Without a cache, a single
// streamed event (and especially the burst released when a permission is
// allowed) re-parses the whole conversation's markdown synchronously, freezing
// the main thread. Each caller gets its own cache via a fresh useMarkdown().
import { ref } from 'vue'
import type MarkdownIt from 'markdown-it'
import { applyMermaidFence } from '@/lib/mermaid'

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

export function useMarkdown() {
  const mdReady = ref(false)
  /** True when the markdown renderer failed to load (e.g. its lazy chunk 404'd).
   * Surfaced so a caller can warn + offer a retry instead of silently falling
   * back to escaped text — a partial/broken deploy must not fail quietly. */
  const mdError = ref(false)
  let _md: MarkdownIt | null = null
  const _mdCache = new Map<string, string>()
  const MD_CACHE_MAX = 800

  /**
   * Render markdown, memoised by source text.
   *
   * `skipCache` is for the entry currently being streamed: its text grows by a
   * chunk every frame, so every render produces a brand-new key. Caching those
   * fills the map with strings that will never be read again and evicts the
   * finalized entries that actually get re-read on scroll.
   */
  function renderText(md: string, skipCache = false): string {
    // Read mdReady so this is reactive to renderer load.
    if (!mdReady.value || !_md) return escapeHtml(md ?? '').replace(/\n/g, '<br/>')
    const key = md ?? ''
    if (skipCache) return _md.render(key)
    const cached = _mdCache.get(key)
    if (cached !== undefined) return cached
    const html = _md.render(key)
    // Evict ONE oldest entry, not the whole map. Clearing wholesale meant the
    // next render had to re-parse the entire conversation from scratch — the
    // periodic freeze during long runs. Map preserves insertion order, so the
    // first key is the oldest; FIFO is enough here because a stream is
    // append-only and older entries are only re-read when scrolling back.
    if (_mdCache.size >= MD_CACHE_MAX) {
      const oldest = _mdCache.keys().next().value
      if (oldest !== undefined) _mdCache.delete(oldest)
    }
    _mdCache.set(key, html)
    return html
  }

  async function loadMarkdown() {
    if (_md) return
    try {
      const MarkdownIt = (await import('markdown-it')).default
      _md = new MarkdownIt({ html: false, linkify: true, typographer: true, breaks: true })
      applyMermaidFence(_md)
      mdReady.value = true
      mdError.value = false
    } catch {
      // leave _md null; renderText falls back to escaped text, but flag it so
      // the UI can surface the failure and offer a retry.
      mdError.value = true
    }
  }

  /** Force a fresh load attempt after a failure (e.g. user tapped "retry"). */
  async function retryLoadMarkdown() {
    mdError.value = false
    await loadMarkdown()
  }

  return { mdReady, mdError, renderText, loadMarkdown, retryLoadMarkdown }
}
