<script setup lang="ts">
// Menu anchored to a POINT (a right-click) instead of to a trigger element.
//
// Same panel and same items as DropdownMenu — the app should not have two
// different-looking menus depending on whether you clicked a ⋯ button or
// right-clicked a row. Only the way it is summoned differs, so only that lives
// here; everything inside is the shared DropdownItem / DropdownSeparator.
//
// Teleported to <body> so it floats above drawers and modals, and clamped to the
// viewport from its MEASURED size — a long menu opened near the bottom edge
// slides up to fit rather than running off-screen.
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'

const props = defineProps<{ open: boolean; x: number; y: number }>()
const emit = defineEmits<{ close: [] }>()

const EDGE_MARGIN = 8

const panel = ref<HTMLElement | null>(null)
const pos = ref({ x: props.x, y: props.y })

async function place() {
  pos.value = { x: props.x, y: props.y }
  await nextTick()
  const el = panel.value
  if (!el) return
  const { width, height } = el.getBoundingClientRect()
  pos.value = {
    x: Math.max(EDGE_MARGIN, Math.min(props.x, window.innerWidth - width - EDGE_MARGIN)),
    y: Math.max(EDGE_MARGIN, Math.min(props.y, window.innerHeight - height - EDGE_MARGIN)),
  }
}

// Dismiss on outside interaction / Escape. Pointerdowns INSIDE the panel are
// ignored: this is a capture-phase handler, so closing here would tear the menu
// down before the item's click fires and the action would never run.
function onGlobalPointer(e: PointerEvent) {
  if (panel.value && e.target instanceof Node && panel.value.contains(e.target)) return
  emit('close')
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') emit('close')
}

function bind(on: boolean) {
  const fn = on ? window.addEventListener : window.removeEventListener
  fn('pointerdown', onGlobalPointer as EventListener, true)
  fn('keydown', onKeydown as EventListener, true)
}

watch(
  () => [props.open, props.x, props.y] as const,
  ([open], prev) => {
    if (open) place()
    if (open !== prev?.[0]) bind(!!open)
  },
  { immediate: true },
)

onBeforeUnmount(() => bind(false))
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      ref="panel"
      class="fixed z-[100] min-w-44 rounded-md border border-border bg-popover p-1 shadow-lg shadow-black/20"
      :style="{ left: `${pos.x}px`, top: `${pos.y}px` }"
      @contextmenu.prevent
      @click="emit('close')"
    >
      <slot />
    </div>
  </Teleport>
</template>
