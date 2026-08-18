<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import CyberFox from '@/components/CyberFox.vue'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useLiveRunsStore } from '@/stores/liveRuns'

type CyberFoxState =
  | 'idle'
  | 'thinking'
  | 'loading'
  | 'running'
  | 'success'
  | 'error'
  | 'permission'
  | 'syncing'
  | 'sleep'

interface Position {
  x: number
  y: number
}

const STORAGE_KEY = 'devdy.cyberFox.position.v1'
const RESET_POSITION_EVENT = 'devdy:cyber-fox-reset-position'
const EDGE_PADDING = 12
const DEFAULT_SIZE = 148
const DEFAULT_RIGHT = 92
const DEFAULT_BOTTOM = 20

type CyberFoxSize = 'sm' | 'md' | 'lg'

const appSettings = useAppSettingsStore()
const live = useLiveRunsStore()
const rootRef = ref<HTMLElement | null>(null)
const ready = ref(false)
const dragging = ref(false)
const position = ref<Position>({ x: 0, y: 0 })

let dragPointerId: number | null = null
let dragStart: Position | null = null
let dragOrigin: Position | null = null
let transientTimer: ReturnType<typeof setTimeout> | null = null
const shownDoneSignals = new Set<string>()
const transientState = ref<CyberFoxState | null>(null)

const enabled = computed(() => appSettings.settings?.cyber_fox_enabled !== 'false')
const mascotSize = computed<CyberFoxSize>(() => {
  const size = appSettings.settings?.cyber_fox_size
  return size === 'sm' || size === 'lg' ? size : 'md'
})

function dimensions() {
  const rect = rootRef.value?.getBoundingClientRect()
  return {
    width: rect?.width || DEFAULT_SIZE,
    height: rect?.height || Math.round(DEFAULT_SIZE / 0.9),
  }
}

function clamp(pos: Position): Position {
  const { width, height } = dimensions()
  const maxX = Math.max(EDGE_PADDING, window.innerWidth - width - EDGE_PADDING)
  const maxY = Math.max(EDGE_PADDING, window.innerHeight - height - EDGE_PADDING)
  return {
    x: Math.min(Math.max(pos.x, EDGE_PADDING), maxX),
    y: Math.min(Math.max(pos.y, EDGE_PADDING), maxY),
  }
}

function defaultPosition(): Position {
  const { width, height } = dimensions()
  return clamp({
    x: window.innerWidth - width - DEFAULT_RIGHT,
    y: window.innerHeight - height - DEFAULT_BOTTOM,
  })
}

function loadPosition(): Position | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<Position>
    if (typeof parsed.x !== 'number' || typeof parsed.y !== 'number') return null
    return clamp({ x: parsed.x, y: parsed.y })
  } catch {
    return null
  }
}

function savePosition() {
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        x: Math.round(position.value.x),
        y: Math.round(position.value.y),
      }),
    )
  } catch {
    /* Storage is best-effort; the mascot still works for this session. */
  }
}

function moveTo(next: Position, persist = false) {
  position.value = clamp(next)
  if (persist) savePosition()
}

function onResize() {
  moveTo(position.value, true)
}

function resetPosition() {
  moveTo(defaultPosition(), true)
}

function startDrag(e: PointerEvent) {
  if (e.button !== 0) return
  dragging.value = true
  dragPointerId = e.pointerId
  dragStart = { x: e.clientX, y: e.clientY }
  dragOrigin = { ...position.value }
  ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
}

function onDrag(e: PointerEvent) {
  if (!dragging.value || dragPointerId !== e.pointerId || !dragStart || !dragOrigin) return
  e.preventDefault()
  moveTo({
    x: dragOrigin.x + e.clientX - dragStart.x,
    y: dragOrigin.y + e.clientY - dragStart.y,
  })
}

function endDrag(e: PointerEvent) {
  if (!dragging.value || dragPointerId !== e.pointerId) return
  dragging.value = false
  dragPointerId = null
  dragStart = null
  dragOrigin = null
  ;(e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId)
  savePosition()
}

function onKeydown(e: KeyboardEvent) {
  const step = e.shiftKey ? 24 : 8
  if (e.key === 'ArrowLeft') {
    e.preventDefault()
    moveTo({ x: position.value.x - step, y: position.value.y }, true)
  } else if (e.key === 'ArrowRight') {
    e.preventDefault()
    moveTo({ x: position.value.x + step, y: position.value.y }, true)
  } else if (e.key === 'ArrowUp') {
    e.preventDefault()
    moveTo({ x: position.value.x, y: position.value.y - step }, true)
  } else if (e.key === 'ArrowDown') {
    e.preventDefault()
    moveTo({ x: position.value.x, y: position.value.y + step }, true)
  }
}

const steadyState = computed<CyberFoxState>(() => {
  let hasRunning = false
  let hasPermission = false
  live.sessions.forEach((session) => {
    if (session.permissionQueue.length > 0) hasPermission = true
    if (session.status === 'running') hasRunning = true
  })
  if (hasPermission) return 'permission'
  if (hasRunning) return 'running'
  return 'idle'
})

const doneSignal = computed(() => {
  for (const session of live.sessions.values()) {
    if (!session.notifyDone || session.status === 'running') continue
    return `${session.runId}:${session.status}`
  }
  return ''
})

watch(
  doneSignal,
  (signal) => {
    if (!signal || shownDoneSignals.has(signal)) return
    shownDoneSignals.add(signal)
    const status = signal.slice(signal.lastIndexOf(':') + 1)
    transientState.value = status === 'failed' || status === 'cancelled' ? 'error' : 'success'
    if (transientTimer) clearTimeout(transientTimer)
    transientTimer = setTimeout(() => {
      transientState.value = null
      transientTimer = null
    }, 1400)
  },
  { immediate: true },
)

const displayState = computed<CyberFoxState>(() => {
  if (steadyState.value === 'permission') return 'permission'
  return transientState.value ?? steadyState.value
})

watch([enabled, mascotSize], async ([isEnabled]) => {
  if (!ready.value || !isEnabled) return
  await nextTick()
  requestAnimationFrame(() => moveTo(position.value, true))
})

const rootStyle = computed(() => ({
  transform: `translate3d(${Math.round(position.value.x)}px, ${Math.round(position.value.y)}px, 0)`,
  opacity: ready.value ? '1' : '0',
}))

onMounted(() => {
  appSettings.ensureLoaded().catch(() => {})
  requestAnimationFrame(() => {
    position.value = loadPosition() ?? defaultPosition()
    ready.value = true
    window.addEventListener('resize', onResize)
    window.addEventListener(RESET_POSITION_EVENT, resetPosition)
  })
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', onResize)
  window.removeEventListener(RESET_POSITION_EVENT, resetPosition)
  if (transientTimer) clearTimeout(transientTimer)
})
</script>

<template>
  <div
    v-if="enabled"
    ref="rootRef"
    class="cyber-fox-floating"
    :class="{ 'cyber-fox-floating--dragging': dragging }"
    :style="rootStyle"
    tabindex="0"
    role="img"
    aria-label="DY Cyber Fox"
    aria-roledescription="draggable mascot"
    title="DY"
    @pointerdown="startDrag"
    @pointermove="onDrag"
    @pointerup="endDrag"
    @pointercancel="endDrag"
    @keydown="onKeydown"
    @dragstart.prevent
  >
    <CyberFox :state="displayState" :size="mascotSize" />
  </div>
</template>

<style scoped>
.cyber-fox-floating {
  position: fixed;
  top: 0;
  left: 0;
  z-index: 30;
  cursor: grab;
  touch-action: none;
  transition: opacity 160ms ease, filter 160ms ease;
  will-change: transform;
}

.cyber-fox-floating:focus-visible {
  outline: 2px solid hsl(var(--ring));
  outline-offset: 4px;
  border-radius: 999px;
}

.cyber-fox-floating:hover {
  filter: saturate(1.08) brightness(1.04);
}

.cyber-fox-floating--dragging {
  cursor: grabbing;
  filter: saturate(1.12) brightness(1.08);
}

@media (prefers-reduced-motion: reduce) {
  .cyber-fox-floating {
    transition: none;
  }
}
</style>
