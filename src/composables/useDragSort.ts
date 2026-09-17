import { computed, onBeforeUnmount, ref, type Ref } from 'vue'

/**
 * Pointer-driven drag-to-reorder for a list or grid of items.
 *
 * Tauri's webview reserves native HTML5 drag-and-drop for OS file drops, which
 * swallows dragstart/dragover, so reordering has to run on raw pointer events.
 *
 * The list's own order is NOT touched while a drag is in flight — only
 * transforms move. The dragged item tracks the pointer 1:1 with no transition;
 * the items it displaces slide to the slot they would occupy once it is gone.
 * Reordering the array live instead would fight the pointer: the layout would
 * shift under the item on every swap, and the others would jump rather than
 * slide (a flex/grid reorder is not a transitionable property). The one real
 * reorder happens on drop, via `onDrop`.
 *
 * Geometry is measured once, when the drag starts, and the landing slot is the
 * resting slot whose centre is nearest — since those centres never move for the
 * duration of the drag, the target cannot flip-flop between two items, which a
 * "swap whenever the pointer is over another item" rule does as soon as items
 * differ in size. Slot offsets are read from the measured rects rather than
 * assumed uniform, so this works for a wrapping grid as well as a single row.
 */

interface Rect {
  left: number
  top: number
  width: number
  height: number
}

export interface UseDragSortOptions {
  /** Element containing the sortable items. */
  container: Ref<HTMLElement | null>
  /** Commit the reorder. Called on drop only when the item changed slot. */
  onDrop: (from: number, to: number) => void
  /** Selects the items inside `container`, in DOM order. */
  itemSelector?: string
  /** Pointer travel (px) before a press counts as a drag rather than a click. */
  threshold?: number
  /** How long a displaced item takes to slide to its new slot. */
  slideMs?: number
}

export function useDragSort(options: UseDragSortOptions) {
  const { container, onDrop } = options
  const itemSelector = options.itemSelector ?? '[data-drag-index]'
  const threshold = options.threshold ?? 4
  const slideMs = options.slideMs ?? 160

  /** Index of the item being dragged, or null when no drag is in flight. */
  const dragFrom = ref<number | null>(null)
  /** Index the dragged item would land on if dropped right now. */
  const dragTo = ref<number | null>(null)
  const dragX = ref(0)
  const dragY = ref(0)

  let rects: Rect[] = []
  /** Where inside the item the pointer grabbed, so it doesn't snap to a corner. */
  let grabDx = 0
  let grabDy = 0
  let pressed: { index: number; x: number; y: number } | null = null
  /** Set once a press turns into a drag; read to swallow the trailing click. */
  let dragged = false

  const isDragging = computed(() => dragFrom.value !== null)

  function measure(index: number, x: number, y: number): boolean {
    const els = Array.from(container.value?.querySelectorAll<HTMLElement>(itemSelector) ?? [])
    rects = els.map((el) => {
      const r = el.getBoundingClientRect()
      return { left: r.left, top: r.top, width: r.width, height: r.height }
    })
    const self = rects[index]
    if (!self) return false
    grabDx = x - self.left
    grabDy = y - self.top
    return true
  }

  function onMove(e: PointerEvent) {
    if (!pressed) return
    if (dragFrom.value === null) {
      const moved =
        Math.abs(e.clientX - pressed.x) >= threshold || Math.abs(e.clientY - pressed.y) >= threshold
      if (!moved) return
      if (!measure(pressed.index, pressed.x, pressed.y)) return
      dragFrom.value = pressed.index
      dragTo.value = pressed.index
      dragged = true
      document.body.style.userSelect = 'none'
    }

    const from = dragFrom.value
    const self = rects[from]
    if (!self) return

    // Follow the pointer, bounded by the span the items occupy.
    let minLeft = Infinity
    let minTop = Infinity
    let maxRight = -Infinity
    let maxBottom = -Infinity
    for (const r of rects) {
      minLeft = Math.min(minLeft, r.left)
      minTop = Math.min(minTop, r.top)
      maxRight = Math.max(maxRight, r.left + r.width)
      maxBottom = Math.max(maxBottom, r.top + r.height)
    }
    const left = clamp(e.clientX - grabDx, minLeft, maxRight - self.width)
    const top = clamp(e.clientY - grabDy, minTop, maxBottom - self.height)
    dragX.value = left - self.left
    dragY.value = top - self.top

    const cx = left + self.width / 2
    const cy = top + self.height / 2
    let best = from
    let bestDist = Infinity
    rects.forEach((r, i) => {
      const dx = r.left + r.width / 2 - cx
      const dy = r.top + r.height / 2 - cy
      const dist = dx * dx + dy * dy
      if (dist < bestDist) {
        bestDist = dist
        best = i
      }
    })
    dragTo.value = best
  }

  /**
   * Inline style for the item at `index`. Returns undefined when idle, so the
   * element carries no transform outside a drag.
   */
  function itemStyle(index: number) {
    const from = dragFrom.value
    if (from === null) return undefined
    if (index === from) {
      return {
        transform: `translate(${dragX.value}px, ${dragY.value}px)`,
        transition: 'none',
        zIndex: '30',
      }
    }
    // The slot this item takes while the dragged one is lifted out of the list.
    const to = dragTo.value ?? from
    let slot = index
    if (to > from && index > from && index <= to) slot = index - 1
    else if (to < from && index >= to && index < from) slot = index + 1
    const here = rects[index]
    const there = rects[slot]
    const dx = here && there ? there.left - here.left : 0
    const dy = here && there ? there.top - here.top : 0
    return {
      transform: `translate(${dx}px, ${dy}px)`,
      transition: `transform ${slideMs}ms ease-out`,
    }
  }

  function cancel() {
    window.removeEventListener('pointermove', onMove)
    window.removeEventListener('pointerup', end)
    window.removeEventListener('pointercancel', cancel)
    document.body.style.userSelect = ''
    pressed = null
    dragFrom.value = null
    dragTo.value = null
    dragX.value = 0
    dragY.value = 0
  }

  function end() {
    const from = dragFrom.value
    const to = dragTo.value
    cancel()
    if (from !== null && to !== null && from !== to) onDrop(from, to)
  }

  /** Call from `@pointerdown` on the item (or its drag handle). */
  function start(index: number, e: PointerEvent) {
    if (e.button !== 0) return
    pressed = { index, x: e.clientX, y: e.clientY }
    dragged = false
    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', end)
    window.addEventListener('pointercancel', cancel)
  }

  /**
   * Whether the click now firing is the tail of a drag and should be ignored
   * rather than treated as activating the item. Consumes the flag.
   */
  function clickWasDrag(): boolean {
    if (!dragged) return false
    dragged = false
    return true
  }

  onBeforeUnmount(cancel)

  return { dragFrom, dragTo, isDragging, start, itemStyle, clickWasDrag, cancel }
}

function clamp(v: number, min: number, max: number): number {
  return Math.min(Math.max(v, min), max)
}
