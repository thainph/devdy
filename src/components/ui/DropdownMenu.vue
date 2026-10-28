<script setup lang="ts">
// Lightweight, on-brand dropdown. Provide the trigger via #trigger slot and
// menu items (DropdownItem) in the default slot. Closes on outside click,
// Escape, or after an item is clicked. Emits `update:open` so callers can keep
// the trigger visible while open.
//
// The panel is teleported to <body> and positioned `fixed` against the trigger,
// so a scroll container (`overflow-auto`) or a clipping rail around the trigger
// can't cut it off. It opens below the trigger, flips above when its MEASURED
// height doesn't fit below, and is clamped to the viewport either way — a tall
// menu (e.g. a PR session's actions) stays fully visible.
import { ref, nextTick, onBeforeUnmount } from 'vue'

const props = withDefaults(defineProps<{ align?: 'left' | 'right' }>(), { align: 'right' })
const emit = defineEmits<{ (e: 'update:open', open: boolean): void }>()

const GAP = 6
const EDGE_MARGIN = 8

const open = ref(false)
const root = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)
const pos = ref({ x: 0, y: 0 })

async function place() {
  const anchor = root.value?.getBoundingClientRect()
  if (!anchor) return
  // First paint below the trigger (hidden by the transition's opacity), then
  // correct from the panel's real size.
  pos.value = { x: anchor.left, y: anchor.bottom + GAP }
  await nextTick()
  const el = panel.value
  if (!el) return
  const { width, height } = el.getBoundingClientRect()
  const vw = window.innerWidth
  const vh = window.innerHeight
  let x = props.align === 'right' ? anchor.right - width : anchor.left
  x = Math.max(EDGE_MARGIN, Math.min(x, vw - width - EDGE_MARGIN))
  const below = anchor.bottom + GAP
  const above = anchor.top - GAP - height
  let y = below
  if (below + height > vh - EDGE_MARGIN) {
    // Doesn't fit below: go above if it fits there, else whichever side has
    // more room, clamped so the whole panel stays on-screen.
    y = above >= EDGE_MARGIN || anchor.top > vh - anchor.bottom ? above : below
    y = Math.max(EDGE_MARGIN, Math.min(y, vh - height - EDGE_MARGIN))
  }
  pos.value = { x, y }
}

function setOpen(v: boolean) {
  if (open.value === v) return
  open.value = v
  bind(v)
  emit('update:open', v)
  if (v) place()
}
function toggle() { setOpen(!open.value) }
function close() { setOpen(false) }

function inside(t: EventTarget | null) {
  return t instanceof Node && (!!root.value?.contains(t) || !!panel.value?.contains(t))
}
function onDoc(e: Event) {
  if (!inside(e.target)) close()
}
function onKey(e: Event) {
  if ((e as KeyboardEvent).key === 'Escape') close()
}
// Follow the trigger when something around it scrolls (a live run's stream
// auto-scrolls) or the window resizes, rather than closing under the user.
let rafId = 0
function reposition(e?: Event) {
  if (e && inside(e.target)) return // scrolling the menu's own list
  if (rafId) return
  rafId = requestAnimationFrame(() => {
    rafId = 0
    if (open.value) place()
  })
}

function bind(on: boolean) {
  const m = on ? 'addEventListener' : 'removeEventListener'
  document[m]('mousedown', onDoc)
  document[m]('keydown', onKey)
  document[m]('scroll', reposition, true)
  window[m]('resize', reposition)
}
onBeforeUnmount(() => {
  bind(false)
  cancelAnimationFrame(rafId)
})
</script>

<template>
  <div ref="root" class="relative inline-flex">
    <span class="inline-flex" @click="toggle">
      <slot name="trigger" :open="open" />
    </span>
    <Teleport to="body">
      <transition
        enter-active-class="transition-opacity duration-150 ease-out"
        enter-from-class="opacity-0"
        leave-active-class="transition-opacity duration-100 ease-in"
        leave-to-class="opacity-0"
      >
        <div
          v-if="open"
          ref="panel"
          class="fixed z-[100] min-w-44 rounded-md border border-border bg-popover p-1 shadow-lg shadow-black/20"
          :style="{ left: `${pos.x}px`, top: `${pos.y}px` }"
          @click="close"
        >
          <slot />
        </div>
      </transition>
    </Teleport>
  </div>
</template>
