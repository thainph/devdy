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
import { getCurrentWindow, currentMonitor, availableMonitors } from '@tauri-apps/api/window'
import { LogicalPosition, PhysicalPosition } from '@tauri-apps/api/dpi'
import CyberFox from '@/components/CyberFoxCanvas.vue'
import MascotStars from '@/components/MascotStars.vue'
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
import type { MascotRealmId } from '@/lib/mascotLevel'
import { setSpeaking } from '@/composables/useMascotSpeaking'

const POSITION_KEY = 'devdy.mascotWindow.position.v1'
const EDGE_MARGIN = 24

const { t } = useI18n()

const state = ref<CyberFoxState>('idle')
const size = ref<CyberFoxSize>('md')
const streams = ref(1)
const lite = ref(false)
const evolutionRealm = ref<MascotRealmId | ''>('')
const evolutionTier = ref(1)
const stars = ref(0)
const levelUpAt = ref(0)
const bubbleMsg = ref<MascotBubbleMessage | null>(null)

const menuOpen = ref(false)
const menuPos = ref({ x: 0, y: 0 })

const menuItems = computed<MascotMenuItem[]>(() => [
  { key: 'todo', label: t('todos.quick.newTodo'), icon: ListTodo },
  { key: 'note', label: t('todos.quick.newNote'), icon: StickyNote },
])

function openContextMenu(e: MouseEvent) {
  menuPos.value = { x: e.clientX, y: e.clientY }
  menuOpen.value = true
}
function pickQuickCreate(key: string) {
  openQuickCreateWindow(key as QuickCreateTab)
}

// Drag = OS-native window move via startDragging(). The compositor moves the
// whole window surface, so it's perfectly smooth and NEVER drops the canvas
// backing — no flicker, unlike driving setPosition on every pointermove. During
// an OS drag we don't get pointerup, so the position is persisted by a debounced
// onMoved listener (see onMounted), which also rescues the window on-screen.
async function startDrag(e: PointerEvent) {
  if (e.button !== 0) return
  menuOpen.value = false
  try {
    await getCurrentWindow().startDragging()
  } catch {
    /* not in a Tauri shell */
  }
}

// Called (debounced) once the window stops moving: keep it on the desktop, save,
// and guarantee the fox is repainted. A real `resize` event makes CyberFoxCanvas
// reallocate the canvas backing (restores it even if macOS dropped it during the
// move); the window is stationary by now so the single reframe isn't visible.
async function afterMove() {
  await rescueOntoDesktop()
  savePosition()
  window.dispatchEvent(new Event('resize'))
}

// Keep the window within the UNION of all monitors (so dragging onto a second
// screen is allowed, but it can never be lost entirely off the desktop). Done in
// physical coordinates to stay correct across monitors with different DPRs.
async function rescueOntoDesktop() {
  const win = getCurrentWindow()
  try {
    const monitors = await availableMonitors()
    if (!monitors.length) return
    const pos = await win.outerPosition() // physical
    const size = await win.outerSize() // physical
    let minX = Infinity
    let minY = Infinity
    let maxX = -Infinity
    let maxY = -Infinity
    for (const m of monitors) {
      minX = Math.min(minX, m.position.x)
      minY = Math.min(minY, m.position.y)
      maxX = Math.max(maxX, m.position.x + m.size.width)
      maxY = Math.max(maxY, m.position.y + m.size.height)
    }
    const margin = 24
    const clampMaxX = maxX - size.width - margin
    const clampMaxY = maxY - size.height - margin
    const x = Math.min(Math.max(pos.x, minX + margin), Math.max(minX + margin, clampMaxX))
    const y = Math.min(Math.max(pos.y, minY + margin), Math.max(minY + margin, clampMaxY))
    if (Math.round(x) !== Math.round(pos.x) || Math.round(y) !== Math.round(pos.y)) {
      await win.setPosition(new PhysicalPosition(Math.round(x), Math.round(y)))
    }
  } catch {
    /* best-effort */
  }
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

// Keep the window fully on its current monitor after a size change (the size
// preference can grow the window past the screen edge). Clamps the CURRENT
// position rather than restoring the saved one.
async function clampIntoView() {
  const win = getCurrentWindow()
  try {
    const monitor = await currentMonitor()
    if (!monitor) return
    const factor = await win.scaleFactor()
    const wSize = (await win.outerSize()).toLogical(factor)
    const pos = (await win.outerPosition()).toLogical(factor)
    const mPos = monitor.position.toLogical(factor)
    const mSize = monitor.size.toLogical(factor)
    const maxX = mPos.x + mSize.width - wSize.width - EDGE_MARGIN
    const maxY = mPos.y + mSize.height - wSize.height - EDGE_MARGIN
    const x = Math.min(Math.max(pos.x, mPos.x + EDGE_MARGIN), Math.max(mPos.x + EDGE_MARGIN, maxX))
    const y = Math.min(Math.max(pos.y, mPos.y + EDGE_MARGIN), Math.max(mPos.y + EDGE_MARGIN, maxY))
    if (Math.round(x) !== Math.round(pos.x) || Math.round(y) !== Math.round(pos.y)) {
      await win.setPosition(new LogicalPosition(Math.round(x), Math.round(y)))
      savePosition()
    }
  } catch {
    /* best-effort */
  }
}

let unlistenState: UnlistenFn | null = null
let unlistenBubble: UnlistenFn | null = null
let unlistenSpeaking: UnlistenFn | null = null
let unlistenResized: UnlistenFn | null = null
let unlistenMoved: UnlistenFn | null = null
let moveSaveTimer: ReturnType<typeof setTimeout> | null = null

function applyPayload(p: Partial<MascotStatePayload>) {
  if (p.state) state.value = p.state
  if (p.size) size.value = p.size
  if (typeof p.streams === 'number') streams.value = Math.max(1, p.streams)
  if (typeof p.lite === 'boolean') lite.value = p.lite
  if (typeof p.evolutionRealm === 'string') evolutionRealm.value = p.evolutionRealm as MascotRealmId | ''
  if (typeof p.evolutionTier === 'number') evolutionTier.value = p.evolutionTier
  if (typeof p.stars === 'number') stars.value = Math.max(0, p.stars)
  if (typeof p.levelUpAt === 'number' && p.levelUpAt > levelUpAt.value) levelUpAt.value = p.levelUpAt
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

  // Re-clamp into view whenever the window is resized (e.g. the size preference
  // changed from the main window), so a bigger fox never spills off-screen.
  try {
    unlistenResized = await getCurrentWindow().onResized(() => {
      void clampIntoView()
    })
  } catch {
    /* not in a Tauri shell */
  }

  // OS-native drag gives us no pointerup, so persist the position (and rescue the
  // window on-screen) a moment after movement settles.
  try {
    unlistenMoved = await getCurrentWindow().onMoved(() => {
      // Repaint the fox on every move tick so its canvas never goes blank mid-drag
      // (macOS drops the backing of a transparent window's canvas as it moves).
      window.dispatchEvent(new Event('devdy:mascot-redraw'))
      if (moveSaveTimer) clearTimeout(moveSaveTimer)
      moveSaveTimer = setTimeout(() => {
        moveSaveTimer = null
        void afterMove()
      }, 250)
    })
  } catch {
    /* not in a Tauri shell */
  }

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
  if (moveSaveTimer) clearTimeout(moveSaveTimer)
  unlistenState?.()
  unlistenBubble?.()
  unlistenSpeaking?.()
  unlistenResized?.()
  unlistenMoved?.()
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
      @contextmenu.prevent="openContextMenu"
      @dragstart.prevent
    >
      <MascotBubble :message="bubbleMsg" placement="bottom" />
      <div class="fox-stack">
        <CyberFox
          :state="state"
          :size="size"
          :streams="streams"
          :lite="lite"
          :evolution-realm="evolutionRealm"
          :evolution-tier="evolutionTier"
          :level-up-at="levelUpAt"
          :persistent="true"
        />
        <MascotStars class="fox-stars" :count="stars" />
      </div>
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
.fox-stack {
  position: relative;
  display: inline-flex;
}
.fox-stars {
  position: absolute;
  top: -2px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 2;
}

.mascot-root {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  /* Anchor the fox to the TOP so it can be dragged to the very top of the screen
     (just under the menu bar). The reserved zone below it holds the speech bubble
     — which now renders below the fox (MascotBubble placement="bottom"). */
  justify-content: flex-start;
  flex-direction: column;
  padding-top: 10px;
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
