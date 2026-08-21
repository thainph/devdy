<script setup lang="ts">
// App-wide host that decides HOW the DY Cyber Fox is shown:
//   • in-app  → render the draggable floating mascot inside this window.
//   • desktop → open a frameless, always-on-top "desktop pet" window and push
//               the live mascot state to it over Tauri events.
// The live-runs store lives here (main window), so this component is the single
// source of truth in both modes; the desktop-pet window is a dumb renderer.
import { onBeforeUnmount, onMounted, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import CyberFoxFloating from '@/components/CyberFoxFloating.vue'
import { useMascotState } from '@/composables/useMascotState'
import { useMascotBubble } from '@/composables/useMascotBubble'
import { useMascotBubbleFeed } from '@/composables/useMascotBubbleFeed'
import {
  MASCOT_READY_EVENT,
  closeMascotWindow,
  emitMascotBubble,
  emitMascotState,
  openMascotWindow,
} from '@/lib/mascotWindow'

const { enabled, mode, mascotSize, displayState, runningCount } = useMascotState()

// Feed app-wide signals (run phase + toasts) into the shared speech bubble.
useMascotBubbleFeed(displayState)
const { state: bubble } = useMascotBubble()

let unlistenReady: UnlistenFn | null = null

function currentPayload() {
  return { state: displayState.value, size: mascotSize.value, streams: runningCount.value }
}

async function ensureDesktopWindow() {
  if (!enabled.value || mode.value !== 'desktop') return
  try {
    console.info('[mascot] opening desktop pet window, size =', mascotSize.value)
    await openMascotWindow(mascotSize.value)
    await emitMascotState(currentPayload())
  } catch (e) {
    console.error('[mascot] failed to open desktop pet window', e)
  }
}

// Open/close the desktop-pet window as the mode / enabled flag changes.
watch(
  [enabled, mode],
  async ([isEnabled, m], prev) => {
    const wasDesktop = prev?.[0] && prev?.[1] === 'desktop'
    if (isEnabled && m === 'desktop') {
      await ensureDesktopWindow()
    } else if (wasDesktop) {
      await closeMascotWindow()
    }
  },
  { immediate: true },
)

// Keep the pet in sync with live state (only matters while in desktop mode).
watch([displayState, runningCount], () => {
  if (enabled.value && mode.value === 'desktop') emitMascotState(currentPayload())
})

// Forward speech-bubble messages to the pet window while in desktop mode.
watch(
  () => bubble.current?.id,
  () => {
    if (bubble.current && enabled.value && mode.value === 'desktop') {
      emitMascotBubble({ ...bubble.current })
    }
  },
)

// Resize the pet window when the size preference changes.
watch(mascotSize, async () => {
  if (enabled.value && mode.value === 'desktop') await ensureDesktopWindow()
})

onMounted(async () => {
  // When the pet window (re)mounts it announces readiness; replay current state.
  try {
    unlistenReady = await listen(MASCOT_READY_EVENT, () => {
      if (enabled.value && mode.value === 'desktop') emitMascotState(currentPayload())
    })
  } catch {
    /* not in a Tauri shell */
  }
})

onBeforeUnmount(() => {
  unlistenReady?.()
})
</script>

<template>
  <!-- In-app mode renders the floating mascot here; desktop mode renders nothing
       in this window (the pet lives in its own OS window). -->
  <CyberFoxFloating v-if="enabled && mode === 'in-app'" />
</template>
