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
  let _md: MarkdownIt | null = null
  const _mdCache = new Map<string, string>()
  const MD_CACHE_MAX = 800

  function renderText(md: string): string {
    // Read mdReady so this is reactive to renderer load.
    if (!mdReady.value || !_md) return escapeHtml(md ?? '').replace(/\n/g, '<br/>')
    const key = md ?? ''
    const cached = _mdCache.get(key)
    if (cached !== undefined) return cached
    const html = _md.render(key)
    if (_mdCache.size >= MD_CACHE_MAX) _mdCache.clear()
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
    } catch {
      // leave _md null; renderText falls back to escaped text
    }
  }

  return { mdReady, renderText, loadMarkdown }
}
