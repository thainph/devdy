// Copy buttons for rendered markdown.
//
// `vCopyCode` is a Vue directive applied to a markdown host element (the same
// place v-html / v-mermaid live). After each render it injects a small "copy"
// button into:
//   - every fenced code block (`<pre>` … but NOT mermaid's source `<pre>`)
//   - every mermaid diagram (copies the original ```mermaid source, whether the
//     block is still showing source or already rendered to SVG)
//
// Buttons are plain DOM (not Vue components) because they're injected into
// v-html output. The directive re-runs on update; since Vue replaces innerHTML
// on each v-html change, decorations are simply re-applied to the fresh DOM.

import type { Directive } from 'vue'

// lucide-style inline icons (stroke = currentColor) so they match the app.
const COPY_ICON =
  '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>'
const CHECK_ICON =
  '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>'

function makeCopyButton(text: string): HTMLButtonElement {
  const btn = document.createElement('button')
  btn.type = 'button'
  btn.className = 'code-copy-btn'
  btn.title = 'Copy'
  btn.setAttribute('aria-label', 'Copy to clipboard')
  btn.innerHTML = COPY_ICON
  btn.addEventListener('click', async (e) => {
    // Don't let the click bubble to the markdown container's prose handler.
    e.preventDefault()
    e.stopPropagation()
    try {
      await navigator.clipboard.writeText(text)
      btn.classList.add('copied')
      btn.title = 'Copied'
      btn.innerHTML = CHECK_ICON
      window.setTimeout(() => {
        btn.classList.remove('copied')
        btn.title = 'Copy'
        btn.innerHTML = COPY_ICON
      }, 1500)
    } catch {
      /* clipboard unavailable */
    }
  })
  return btn
}

function decorateCopy(el: HTMLElement): void {
  // Fenced code blocks (skip the source <pre> inside a mermaid diagram — the
  // diagram gets its own button below).
  el.querySelectorAll<HTMLElement>('pre').forEach((pre) => {
    if (pre.closest('.mermaid-diagram')) return
    if (pre.dataset.copyDecorated) return
    pre.dataset.copyDecorated = '1'
    if (!pre.style.position) pre.style.position = 'relative'
    const code = pre.querySelector('code')
    const text = (code ?? pre).textContent ?? ''
    if (!text.trim()) return
    pre.appendChild(makeCopyButton(text))
  })

  // Mermaid diagrams — copy the original source from the data attribute.
  el.querySelectorAll<HTMLElement>('.mermaid-diagram').forEach((block) => {
    if (block.dataset.copyDecorated) return
    block.dataset.copyDecorated = '1'
    const src = decodeURIComponent(block.dataset.mermaid ?? '')
    if (!src.trim()) return
    block.appendChild(makeCopyButton(src))
  })
}

export const vCopyCode: Directive<HTMLElement> = {
  mounted: (el) => decorateCopy(el),
  updated: (el) => decorateCopy(el),
}
