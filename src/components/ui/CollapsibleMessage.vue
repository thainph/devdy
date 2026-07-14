<script setup lang="ts">
// Collapses over-long message content behind a height clamp + fade-out + a
// "Show more / Show less" toggle, so a single huge message doesn't force the
// user to scroll the whole conversation. Measures the *natural* content height
// with a ResizeObserver so it keeps working while content streams in (markdown,
// mermaid, images all change height after mount).
import { ref, computed, onMounted, onBeforeUnmount, nextTick } from 'vue'
import { ChevronDown } from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  /** Clamp height in px; content taller than this collapses. */
  maxHeight?: number
  /** CSS color the bottom fade blends into (match the surrounding background). */
  fade?: string
  /** Toggle button alignment. */
  align?: 'left' | 'center' | 'right'
}>(), {
  maxHeight: 384,
  fade: 'hsl(var(--background))',
  align: 'left',
})

const expanded = ref(false)
const overflowing = ref(false)
const contentRef = ref<HTMLElement | null>(null)
const toggleRef = ref<HTMLElement | null>(null)
let ro: ResizeObserver | null = null

// Nearest ancestor that actually scrolls, so we can adjust its scrollTop to
// keep the click point stable when content shrinks. Falls back to the document.
function getScrollParent(el: HTMLElement | null): HTMLElement | null {
  let node = el?.parentElement || null
  while (node) {
    const oy = getComputedStyle(node).overflowY
    if ((oy === 'auto' || oy === 'scroll') && node.scrollHeight > node.clientHeight) return node
    node = node.parentElement
  }
  return (document.scrollingElement as HTMLElement) || null
}

// Smart scroll on toggle:
// - Collapsing shrinks the block above the button, which would yank everything
//   upward and lose the reading position. We pin the toggle button to the exact
//   same viewport Y by compensating scrollTop, so nothing appears to jump.
// - Expanding grows content downward from the clamp; leaving scroll untouched
//   lets the user read on naturally from where the cut was.
function onToggle() {
  const collapsing = expanded.value
  if (!collapsing) {
    expanded.value = true
    return
  }
  const anchorBefore = toggleRef.value?.getBoundingClientRect().top ?? 0
  expanded.value = false
  nextTick(() => {
    const scroller = getScrollParent(contentRef.value)
    const anchorAfter = toggleRef.value?.getBoundingClientRect().top ?? 0
    if (scroller) scroller.scrollTop += anchorAfter - anchorBefore
  })
}

function measure() {
  const el = contentRef.value
  if (!el) return
  // Small tolerance so a message that's a few px over doesn't get a toggle.
  overflowing.value = el.offsetHeight > props.maxHeight + 24
}

const collapsed = computed(() => overflowing.value && !expanded.value)
const alignClass = computed(() =>
  props.align === 'center' ? 'justify-center' : props.align === 'right' ? 'justify-end' : 'justify-start',
)

onMounted(() => {
  measure()
  ro = new ResizeObserver(() => measure())
  if (contentRef.value) ro.observe(contentRef.value)
})
onBeforeUnmount(() => ro?.disconnect())
</script>

<template>
  <div>
    <div
      class="relative"
      :style="collapsed ? { maxHeight: `${maxHeight}px`, overflow: 'hidden' } : undefined"
    >
      <div ref="contentRef">
        <slot />
      </div>
      <!-- Bottom fade hint that there's more below the clamp. -->
      <div
        v-if="collapsed"
        class="pointer-events-none absolute inset-x-0 bottom-0 h-14"
        :style="{ background: `linear-gradient(to bottom, transparent, ${fade})` }"
      />
    </div>

    <div v-if="overflowing" ref="toggleRef" class="flex mt-1" :class="alignClass">
      <button
        type="button"
        class="inline-flex items-center gap-1 text-xs text-foreground/50 hover:text-foreground transition-colors cursor-pointer"
        @click="onToggle"
      >
        <ChevronDown class="h-3.5 w-3.5 transition-transform" :class="expanded ? 'rotate-180' : ''" :stroke-width="2" />
        {{ expanded ? 'Show less' : 'Show more' }}
      </button>
    </div>
  </div>
</template>
