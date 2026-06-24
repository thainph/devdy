// Zoom & pan for rendered Mermaid diagrams.
//
// Once a `.mermaid-diagram` placeholder has its <svg> swapped in (see
// src/lib/mermaid.ts), this turns the persistent `.mermaid-render` wrapper into
// an interactive viewport:
//   - scroll-wheel zooms toward the cursor
//   - +/- buttons zoom toward the centre; the reset button restores 1:1
//   - dragging pans
//
// Decorations are attached once per diagram. The mermaid directive re-runs on
// every v-html update and only replaces the inner <svg>; the outer
// `.mermaid-diagram`, the `.mermaid-render` viewport and the control toolbar all
// survive, so listeners stay valid and we just re-apply the saved transform to
// the fresh <svg>. State lives in a WeakMap keyed by the diagram element.

interface ZoomState {
  scale: number
  tx: number
  ty: number
}

const MIN_SCALE = 0.3
const MAX_SCALE = 8
const BUTTON_STEP = 1.25
const WHEEL_STEP = 1.0015 // applied as factor ** -deltaY for smooth wheel zoom

const states = new WeakMap<HTMLElement, ZoomState>()

// lucide-style inline icons (stroke = currentColor) so they match the app.
const ZOOM_IN_ICON =
  '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/><line x1="11" x2="11" y1="8" y2="14"/><line x1="8" x2="14" y1="11" y2="11"/></svg>'
const ZOOM_OUT_ICON =
  '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/><line x1="8" x2="14" y1="11" y2="11"/></svg>'
const RESET_ICON =
  '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7V5a2 2 0 0 1 2-2h2"/><path d="M17 3h2a2 2 0 0 1 2 2v2"/><path d="M21 17v2a2 2 0 0 1-2 2h-2"/><path d="M7 21H5a2 2 0 0 1-2-2v-2"/></svg>'

const clamp = (v: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, v))

function getState(block: HTMLElement): ZoomState {
  let s = states.get(block)
  if (!s) {
    s = { scale: 1, tx: 0, ty: 0 }
    states.set(block, s)
  }
  return s
}

function applyTransform(svg: SVGElement | null, s: ZoomState): void {
  if (!svg) return
  svg.style.transformOrigin = '0 0'
  svg.style.transform = `translate(${s.tx}px, ${s.ty}px) scale(${s.scale})`
}

function getSvg(target: HTMLElement): SVGElement | null {
  return target.querySelector<SVGElement>('svg')
}

/**
 * Zoom so the diagram point under (px, py) — viewport-local coordinates — stays
 * fixed while the scale changes to `nextScale`.
 */
function zoomAt(block: HTMLElement, target: HTMLElement, nextScale: number, px: number, py: number): void {
  const s = getState(block)
  const clamped = clamp(nextScale, MIN_SCALE, MAX_SCALE)
  if (clamped === s.scale) return
  const ratio = clamped / s.scale
  s.tx = px - (px - s.tx) * ratio
  s.ty = py - (py - s.ty) * ratio
  s.scale = clamped
  applyTransform(getSvg(target), s)
}

function makeButton(icon: string, title: string, onClick: () => void): HTMLButtonElement {
  const btn = document.createElement('button')
  btn.type = 'button'
  btn.className = 'mermaid-zoom-btn'
  btn.title = title
  btn.setAttribute('aria-label', title)
  btn.innerHTML = icon
  btn.addEventListener('click', (e) => {
    e.preventDefault()
    e.stopPropagation()
    onClick()
  })
  return btn
}

/**
 * Make a single rendered diagram interactive. Safe to call repeatedly — the
 * heavy setup (toolbar, listeners) runs once; later calls just re-apply the
 * saved transform to the (possibly new) <svg>.
 */
export function enableMermaidZoom(block: HTMLElement): void {
  const target = block.querySelector<HTMLElement>('.mermaid-render')
  if (!target) return
  const svg = getSvg(target)
  if (!svg) return // not rendered yet — directive will call again after render

  // Re-apply the saved transform to the current <svg> (it may have been swapped).
  applyTransform(svg, getState(block))

  if (block.dataset.zoomDecorated) return
  block.dataset.zoomDecorated = '1'

  // --- toolbar -------------------------------------------------------------
  const controls = document.createElement('div')
  controls.className = 'mermaid-zoom-controls'

  const zoomByButton = (factor: number) => {
    const rect = target.getBoundingClientRect()
    zoomAt(block, target, getState(block).scale * factor, rect.width / 2, rect.height / 2)
  }
  controls.appendChild(makeButton(ZOOM_IN_ICON, 'Zoom in', () => zoomByButton(BUTTON_STEP)))
  controls.appendChild(makeButton(ZOOM_OUT_ICON, 'Zoom out', () => zoomByButton(1 / BUTTON_STEP)))
  controls.appendChild(
    makeButton(RESET_ICON, 'Reset zoom', () => {
      const s = getState(block)
      s.scale = 1
      s.tx = 0
      s.ty = 0
      applyTransform(getSvg(target), s)
    }),
  )
  block.appendChild(controls)

  // --- wheel zoom ----------------------------------------------------------
  target.addEventListener(
    'wheel',
    (e: WheelEvent) => {
      e.preventDefault()
      const rect = target.getBoundingClientRect()
      const px = e.clientX - rect.left
      const py = e.clientY - rect.top
      const next = getState(block).scale * Math.pow(WHEEL_STEP, -e.deltaY)
      zoomAt(block, target, next, px, py)
    },
    { passive: false },
  )

  // --- drag to pan ---------------------------------------------------------
  let dragging = false
  let startX = 0
  let startY = 0
  let originTx = 0
  let originTy = 0

  target.addEventListener('pointerdown', (e: PointerEvent) => {
    // Ignore clicks on the toolbar buttons.
    if ((e.target as HTMLElement).closest('.mermaid-zoom-controls')) return
    dragging = true
    startX = e.clientX
    startY = e.clientY
    const s = getState(block)
    originTx = s.tx
    originTy = s.ty
    target.setPointerCapture(e.pointerId)
    target.classList.add('is-panning')
  })

  target.addEventListener('pointermove', (e: PointerEvent) => {
    if (!dragging) return
    const s = getState(block)
    s.tx = originTx + (e.clientX - startX)
    s.ty = originTy + (e.clientY - startY)
    applyTransform(getSvg(target), s)
  })

  const endDrag = (e: PointerEvent) => {
    if (!dragging) return
    dragging = false
    try {
      target.releasePointerCapture(e.pointerId)
    } catch {
      /* pointer already released */
    }
    target.classList.remove('is-panning')
  }
  target.addEventListener('pointerup', endDrag)
  target.addEventListener('pointercancel', endDrag)

  // Double-click resets, a familiar shortcut for "fit again".
  target.addEventListener('dblclick', (e) => {
    e.preventDefault()
    const s = getState(block)
    s.scale = 1
    s.tx = 0
    s.ty = 0
    applyTransform(getSvg(target), s)
  })
}

/** Enable zoom/pan on every rendered diagram inside `el`. */
export function enableMermaidZoomIn(el: HTMLElement): void {
  el.querySelectorAll<HTMLElement>('.mermaid-diagram').forEach((block) => enableMermaidZoom(block))
}
