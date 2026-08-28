<script setup lang="ts">
/**
 * DY — Cyber Fox mascot, Canvas 2D renderer.
 * Drop-in replacement for CyberFox.vue: same public props, same visual result,
 * but the whole rig + every effect is drawn onto ONE <canvas> by a single rAF
 * loop (see lib/foxRenderer.ts) instead of dozens of composited DOM layers.
 */
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { mascotSpeaking } from '@/composables/useMascotSpeaking'
import { FoxRenderer, type FoxState, type FoxRealm } from '@/lib/foxRenderer'
import type { CyberFoxSize } from '@/composables/useMascotState'

const SIZES = { sm: 96, md: 148, lg: 196 } as const

const props = withDefaults(
  defineProps<{
    state?: FoxState
    size?: CyberFoxSize | number
    label?: string
    reducedMotion?: boolean
    evolutionRealm?: FoxRealm
    evolutionTier?: number
    streams?: number
    lite?: boolean
    /** Timestamp bump to fire the breakthrough (level-up) ring VFX. */
    levelUpAt?: number
    /** Keep the rAF loop running even when hidden/off-screen (desktop pet). */
    persistent?: boolean
  }>(),
  {
    state: 'idle',
    size: 'md',
    label: '',
    reducedMotion: false,
    evolutionRealm: '',
    evolutionTier: 1,
    streams: 1,
    lite: false,
    levelUpAt: 0,
    persistent: false,
  },
)

const rootRef = ref<HTMLElement | null>(null)
const canvasRef = ref<HTMLCanvasElement | null>(null)
let renderer: FoxRenderer | null = null
let io: IntersectionObserver | null = null
let visible = true
let onScreen = true

function sizePx(): number {
  return typeof props.size === 'number' ? Math.max(64, props.size) : SIZES[props.size]
}

function pushProps() {
  renderer?.setProps({
    state: props.state,
    size: props.size,
    streams: props.streams,
    lite: props.lite,
    reducedMotion: props.reducedMotion,
    evolutionRealm: props.evolutionRealm,
    evolutionTier: props.evolutionTier,
    speaking: mascotSpeaking.value,
    persistent: props.persistent,
  })
}

function updateRunning() {
  if (!renderer) return
  // Pause the loop when the tab/window is hidden or the fox is scrolled off-screen.
  // The desktop pet passes `persistent` so it keeps animating regardless — macOS
  // can leave its canvas blank if the loop pauses while the window is moved.
  if ((props.persistent || (visible && onScreen)) && !props.reducedMotion) renderer.start()
  else renderer.stop()
}

function onVisibility() {
  visible = !document.hidden
  updateRunning()
}

// The window moved/resized or regained focus. On macOS the canvas backing store
// can be dropped when its window is dragged to another monitor — the fox turns
// blank while the DOM stars stay. Force a resize + redraw to bring it back.
function onWindowChange() {
  if (!renderer) return
  renderer.refresh()
  updateRunning()
}

// Fired by MascotWindow on every window-move tick during an OS drag: repaint the
// fox (no resize → no flicker) so it never goes blank while being dragged.
function onExternalRedraw() {
  renderer?.redraw()
}

watch(
  () => [
    props.state,
    props.size,
    props.streams,
    props.lite,
    props.reducedMotion,
    props.evolutionRealm,
    props.evolutionTier,
    mascotSpeaking.value,
  ],
  () => {
    pushProps()
    updateRunning()
  },
)

// Fire the breakthrough VFX whenever the timestamp bumps, and keep the rAF loop
// alive for the effect's duration even if the fox would otherwise be paused.
watch(
  () => props.levelUpAt,
  (v, old) => {
    if (!renderer || !v || v === old || props.reducedMotion) return
    renderer.triggerLevelUp()
    renderer.start()
    window.setTimeout(() => updateRunning(), 2100)
  },
)

onMounted(async () => {
  if (!canvasRef.value) return
  renderer = new FoxRenderer(canvasRef.value)
  await renderer.init()
  pushProps()

  if (typeof IntersectionObserver !== 'undefined' && rootRef.value) {
    io = new IntersectionObserver(
      ([e]) => {
        onScreen = e.isIntersecting
        updateRunning()
      },
      { threshold: 0.01 },
    )
    io.observe(rootRef.value)
  }
  document.addEventListener('visibilitychange', onVisibility)
  window.addEventListener('resize', onWindowChange)
  window.addEventListener('focus', onWindowChange)
  window.addEventListener('devdy:mascot-redraw', onExternalRedraw)
  visible = !document.hidden
  updateRunning()
})

onBeforeUnmount(() => {
  io?.disconnect()
  document.removeEventListener('visibilitychange', onVisibility)
  window.removeEventListener('resize', onWindowChange)
  window.removeEventListener('focus', onWindowChange)
  window.removeEventListener('devdy:mascot-redraw', onExternalRedraw)
  renderer?.destroy()
  renderer = null
})
</script>

<template>
  <div
    ref="rootRef"
    class="cyber-fox-canvas"
    :style="{ width: sizePx() + 'px', height: sizePx() + 'px' }"
    :aria-label="label || undefined"
    :aria-hidden="label ? undefined : 'true'"
    role="img"
  >
    <canvas ref="canvasRef" class="cyber-fox-canvas__c"></canvas>
    <span v-if="label" class="sr-only">{{ label }}</span>
  </div>
</template>

<style scoped>
.cyber-fox-canvas {
  position: relative;
  display: block;
  pointer-events: none;
  user-select: none;
}
.cyber-fox-canvas__c {
  display: block;
  /* NOTE: deliberately NOT promoted to its own GPU layer (no translateZ/
     will-change). On macOS a forced compositing layer for a canvas inside a
     transparent window fails to composite in some screen regions (the fox goes
     blank in the upper half while the DOM stars remain). Left inline, the
     CPU-backed canvas (see willReadFrequently) composites like the DOM does. */
}
.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}
</style>
