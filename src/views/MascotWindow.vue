<script setup lang="ts">
// Bare, transparent host for the DY Cyber Fox "desktop pet" window (opened via
// openMascotWindow / `?mascotWindow=1`). It renders ONLY the fox — no app chrome
// — on a fully transparent, frameless, always-on-top OS window that floats on
// the desktop. Phase/size/streams are pushed from the main window over the
// `mascot:state` event (the main window owns the live-runs store); this window
// is a dumb renderer. Dragging moves the whole OS window; hover reveals the same
// Todo/Note quick-create bubbles as the in-app mascot.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ListTodo, StickyNote } from 'lucide-vue-next'
import { listen, emit, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow, currentMonitor } from '@tauri-apps/api/window'
import { LogicalPosition } from '@tauri-apps/api/dpi'
import CyberFox from '@/components/CyberFox.vue'
import MascotContextMenu, { type MascotMenuItem } from '@/components/MascotContextMenu.vue'
import MascotBubble from '@/components/MascotBubble.vue'
import { openQuickCreateWindow, type QuickCreateTab } from '@/lib/quickCreateWindow'
import {
  MASCOT_STATE_EVENT,
  MASCOT_READY_EVENT,
  MASCOT_BUBBLE_EVENT,
  MASCOT_SPEAKING_EVENT,
  type MascotStatePayload,
} from '@/lib/mascotWindow'
import type { CyberFoxSize, CyberFoxState } from '@/composables/useMascotState'
import type { MascotBubbleMessage } from '@/composables/useMascotBubble'
import { setSpeaking } from '@/composables/useMascotSpeaking'

const POSITION_KEY = 'devdy.mascotWindow.position.v1'
const EDGE_MARGIN = 24

const { t } = useI18n()

const state = ref<CyberFoxState>('idle')
const size = ref<CyberFoxSize>('md')
const streams = ref(1)
const lite = ref(false)
const bubbleMsg = ref<MascotBubbleMessage | null>(null)

const menuOpen = ref(false)
const menuPos = ref({ x: 0, y: 0 })
let dragging = false

const menuItems = computed<MascotMenuItem[]>(() => [
  { key: 'todo', label: t('todos.quick.newTodo'), icon: ListTodo },
  { key: 'note', label: t('todos.quick.newNote'), icon: StickyNote },
])

function openContextMenu(e: MouseEvent) {
  if (dragging) return
  menuPos.value = { x: e.clientX, y: e.clientY }
  menuOpen.value = true
}
function pickQuickCreate(key: string) {
  openQuickCreateWindow(key as QuickCreateTab)
}

// Drag the whole OS window by hand. We anchor to SCREEN coordinates (screenX/Y),
// which stay stable even as the window itself moves under the cursor, and drive
// the window with setPosition. This is more reliable than window.startDragging()
// for a transparent, always-on-top window on macOS.
let dragPointerId: number | null = null
let dragStartScreen: { x: number; y: number } | null = null
let dragStartWin: { x: number; y: number } | null = null

async function startDrag(e: PointerEvent) {
  if (e.button !== 0) return
  menuOpen.value = false
  const win = getCurrentWindow()
  try {
    const factor = await win.scaleFactor()
    const logical = (await win.outerPosition()).toLogical(factor)
    dragStartWin = { x: logical.x, y: logical.y }
  } catch {
    return // not in a Tauri shell
  }
  dragStartScreen = { x: e.screenX, y: e.screenY }
  dragPointerId = e.pointerId
  dragging = true
  ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
}

function onDrag(e: PointerEvent) {
  if (!dragging || dragPointerId !== e.pointerId || !dragStartScreen || !dragStartWin) return
  e.preventDefault()
  const nx = Math.round(dragStartWin.x + (e.screenX - dragStartScreen.x))
  const ny = Math.round(dragStartWin.y + (e.screenY - dragStartScreen.y))
  getCurrentWindow().setPosition(new LogicalPosition(nx, ny)).catch(() => {})
}

function endDrag(e: PointerEvent) {
  if (dragPointerId !== e.pointerId) return
  dragging = false
  dragPointerId = null
  dragStartScreen = null
  dragStartWin = null
  try {
    ;(e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId)
  } catch {
    /* pointer already released */
  }
  savePosition()
}

async function savePosition() {
  try {
    const pos = await getCurrentWindow().outerPosition()
    const factor = await getCurrentWindow().scaleFactor()
    const logical = pos.toLogical(factor)
    localStorage.setItem(
      POSITION_KEY,
      JSON.stringify({ x: Math.round(logical.x), y: Math.round(logical.y) }),
    )
  } catch {
    /* best-effort */
  }
}

function readSaved(): { x: number; y: number } | null {
  try {
    const raw = localStorage.getItem(POSITION_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as { x?: number; y?: number }
    if (typeof parsed.x !== 'number' || typeof parsed.y !== 'number') return null
    if (!Number.isFinite(parsed.x) || !Number.isFinite(parsed.y)) return null
    return { x: parsed.x, y: parsed.y }
  } catch {
    return null
  }
}

async function restorePosition() {
  const win = getCurrentWindow()
  try {
    const monitor = await currentMonitor()
    const factor = await win.scaleFactor()
    const wSize = (await win.outerSize()).toLogical(factor)

    // Fallbacks if we can't read the monitor (keeps the pet on-screen no matter
    // what a stale saved position says).
    let mx = 0
    let my = 0
    let mw = 1280
    let mh = 800
    if (monitor) {
      const mPos = monitor.position.toLogical(factor)
      const mSize = monitor.size.toLogical(factor)
      mx = mPos.x
      my = mPos.y
      mw = mSize.width
      mh = mSize.height
    }

    // Default resting spot: bottom-right of the monitor.
    const defX = mx + mw - wSize.width - EDGE_MARGIN
    const defY = my + mh - wSize.height - EDGE_MARGIN

    const saved = readSaved()
    let x = saved ? saved.x : defX
    let y = saved ? saved.y : defY

    // Clamp so the window is always fully within the monitor's visible bounds —
    // this rescues a pet that a previous session parked off-screen.
    const maxX = mx + mw - wSize.width - EDGE_MARGIN
    const maxY = my + mh - wSize.height - EDGE_MARGIN
    x = Math.min(Math.max(x, mx + EDGE_MARGIN), Math.max(mx + EDGE_MARGIN, maxX))
    y = Math.min(Math.max(y, my + EDGE_MARGIN), Math.max(my + EDGE_MARGIN, maxY))

    await win.setPosition(new LogicalPosition(Math.round(x), Math.round(y)))
  } catch {
    // Last resort: a safe on-screen corner.
    try {
      await win.setPosition(new LogicalPosition(80, 80))
    } catch {
      /* best-effort */
    }
  }
}

let unlistenState: UnlistenFn | null = null
let unlistenBubble: UnlistenFn | null = null
let unlistenSpeaking: UnlistenFn | null = null

function applyPayload(p: Partial<MascotStatePayload>) {
  if (p.state) state.value = p.state
  if (p.size) size.value = p.size
  if (typeof p.streams === 'number') streams.value = Math.max(1, p.streams)
  if (typeof p.lite === 'boolean') lite.value = p.lite
}

onMounted(async () => {
  // Make the whole document transparent so only the fox shows over the desktop.
  document.documentElement.classList.add('mascot-window')

  unlistenState = await listen<MascotStatePayload>(MASCOT_STATE_EVENT, (e) => {
    if (e.payload) applyPayload(e.payload)
  })
  unlistenBubble = await listen<MascotBubbleMessage>(MASCOT_BUBBLE_EVENT, (e) => {
    if (e.payload) bubbleMsg.value = e.payload
  })
  unlistenSpeaking = await listen<boolean>(MASCOT_SPEAKING_EVENT, (e) => {
    setSpeaking(Boolean(e.payload))
  })

  await restorePosition()
  try {
    await getCurrentWindow().show()
  } catch {
    /* ignore */
  }
  // Tell the main window we're ready so it replays the current state at once.
  emit(MASCOT_READY_EVENT).catch(() => {})
})

onBeforeUnmount(() => {
  document.documentElement.classList.remove('mascot-window')
  unlistenState?.()
  unlistenBubble?.()
  unlistenSpeaking?.()
})
</script>

<template>
  <div class="mascot-root">
    <div
      class="mascot-grip"
      role="img"
      aria-label="DY Cyber Fox"
      title="DY — drag to move, right-click for menu"
      @pointerdown="startDrag"
      @pointermove="onDrag"
      @pointerup="endDrag"
      @pointercancel="endDrag"
      @contextmenu.prevent="openContextMenu"
      @dragstart.prevent
    >
      <MascotBubble :message="bubbleMsg" />
      <CyberFox :state="state" :size="size" :streams="streams" :lite="lite" />
    </div>

    <!-- Right-click quick-create menu (Todo / Note). -->
    <MascotContextMenu
      :open="menuOpen"
      :x="menuPos.x"
      :y="menuPos.y"
      :items="menuItems"
      @select="pickQuickCreate"
      @close="menuOpen = false"
    />
  </div>
</template>

<style scoped>
.mascot-root {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  /* Anchor the fox to the bottom so the reserved zone above it holds the
     speech bubble without clipping. */
  justify-content: flex-end;
  flex-direction: column;
  padding-bottom: 16px;
  background: transparent;
  overflow: visible;
}

.mascot-grip {
  position: relative;
  cursor: grab;
  touch-action: none;
  transition: filter 160ms ease;
}
.mascot-grip:hover {
  filter: saturate(1.08) brightness(1.04);
}
.mascot-grip:active {
  cursor: grabbing;
}
</style>
