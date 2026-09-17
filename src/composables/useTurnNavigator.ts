// Jump between the user's own messages in a long conversation.
//
// Reading back through a run means hunting for "where did I ask for this?" —
// the user turns are the landmarks, everything between them is the assistant
// working. This walks the scroll container from landmark to landmark.
//
// The turns are found in the DOM (`[data-user-turn]`, stamped by StreamLog)
// rather than measured from a list of entry heights: StreamLog skips off-screen
// entries with `content-visibility` but reserves their measured height, so a
// rect read is accurate for entries that were never laid out — an offset table
// we maintained ourselves would not be.
//
// Layout is only read on demand (a click, a shortcut, opening the list), never
// on scroll: the output pane already coalesces its own per-frame layout reads to
// stay smooth, and a second reader walking every turn would undo that.
import { computed, type Ref } from 'vue'
import type { StreamEntry } from '@/lib/streamEvents'

export interface PromptTurn {
  /** Index into the `entries` array — matches the `data-user-turn` attribute. */
  entryIndex: number
  /** One-line preview for the jump list; empty when the message was images only. */
  label: string
}

/** Gap kept above a turn after jumping, so the bubble isn't flush to the edge. */
const TOP_GAP = 12
/** Slack around the anchor line so the turn just jumped to isn't picked again. */
const EPS = 4
const LABEL_MAX = 70

function previewOf(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > LABEL_MAX ? `${flat.slice(0, LABEL_MAX)}…` : flat
}

export function useTurnNavigator(
  scrollEl: Ref<HTMLElement | null>,
  entries: Ref<readonly StreamEntry[]>,
) {
  const turns = computed<PromptTurn[]>(() => {
    const out: PromptTurn[] = []
    entries.value.forEach((e, entryIndex) => {
      if (e.kind === 'user') out.push({ entryIndex, label: previewOf(e.text) })
    })
    return out
  })

  // Turn elements in document order, paired with how far below the container's
  // top edge each one currently sits.
  function positions(): { entryIndex: number; el: HTMLElement; top: number }[] {
    const root = scrollEl.value
    if (!root) return []
    const base = root.getBoundingClientRect().top
    return Array.from(root.querySelectorAll<HTMLElement>('[data-user-turn]')).map((el) => ({
      entryIndex: Number(el.dataset.userTurn),
      el,
      top: el.getBoundingClientRect().top - base,
    }))
  }

  function scrollTo(el: HTMLElement) {
    const root = scrollEl.value
    if (!root) return
    // Delta from the container's own rect rather than `offsetTop`: the entry's
    // offsetParent isn't necessarily the scroll container.
    const delta = el.getBoundingClientRect().top - root.getBoundingClientRect().top
    // Jumps land instantly on purpose — smooth-scrolling a long run animates
    // through thousands of pixels, and a second press mid-animation fights it.
    root.scrollTop = Math.max(0, root.scrollTop + delta - TOP_GAP)
  }

  function scrollToTurn(entryIndex: number) {
    const el = scrollEl.value?.querySelector<HTMLElement>(`[data-user-turn="${entryIndex}"]`)
    if (el) scrollTo(el)
  }

  /** The turn the reader is currently parked on, or -1 when above the first. */
  function activeEntryIndex(): number {
    let active = -1
    for (const p of positions()) {
      if (p.top > TOP_GAP + EPS) break
      active = p.entryIndex
    }
    return active
  }

  function goPrev() {
    const above = positions().filter((p) => p.top < TOP_GAP - EPS)
    const target = above[above.length - 1]
    if (target) scrollTo(target.el)
  }

  function goNext() {
    const target = positions().find((p) => p.top > TOP_GAP + EPS)
    if (target) scrollTo(target.el)
  }

  return { turns, scrollToTurn, activeEntryIndex, goPrev, goNext }
}
