<script setup lang="ts">
// App-wide host that decides HOW the DY Cyber Fox is shown:
//   • in-app  → render the draggable floating mascot inside this window.
//   • desktop → open a frameless, always-on-top "desktop pet" window and push
//               the live mascot state to it over Tauri events.
// The live-runs store lives here (main window), so this component is the single
// source of truth in both modes; the desktop-pet window is a dumb renderer.
import { nextTick, onBeforeUnmount, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import CyberFoxFloating from '@/components/CyberFoxFloating.vue'
import { useMascotState } from '@/composables/useMascotState'
import { useMascotBubble } from '@/composables/useMascotBubble'
import { useMascotBubbleFeed } from '@/composables/useMascotBubbleFeed'
import { triggerMascotLevelUp } from '@/composables/useMascotLevelUp'
import { useMascotSound } from '@/composables/useMascotSound'
import { mascotSpeaking } from '@/composables/useMascotSpeaking'
import { useMascotLevelStore } from '@/stores/mascotLevel'
import type { MascotRealmId } from '@/lib/mascotLevel'
import {
  MASCOT_READY_EVENT,
  closeMascotWindow,
  emitMascotBubble,
  emitMascotSpeaking,
  emitMascotState,
  openMascotWindow,
} from '@/lib/mascotWindow'

const { t } = useI18n()
const {
  enabled,
  soundEnabled,
  liteMode,
  mode,
  mascotSize,
  displayState,
  runningCount,
  mascotLevel,
  evolutionRealm,
  evolutionTier,
  mascotStars,
} = useMascotState()

// Feed app-wide signals (run phase + toasts) into the shared speech bubble.
useMascotBubbleFeed(displayState)
const { state: bubble, push } = useMascotBubble()
const sound = useMascotSound()
const levelStore = useMascotLevelStore()

// realm id → i18n label key (matches the Settings realm list).
const REALM_LABEL_KEY: Record<MascotRealmId, string> = {
  luyen_khi: 'settings.mascot.levels.realms.luyenKhi',
  truc_co: 'settings.mascot.levels.realms.trucCo',
  kim_dan: 'settings.mascot.levels.realms.kimDan',
  nguyen_anh: 'settings.mascot.levels.realms.nguyenAnh',
  hoa_than: 'settings.mascot.levels.realms.hoaThan',
  anh_bien: 'settings.mascot.levels.realms.anhBien',
  van_dinh: 'settings.mascot.levels.realms.vanDinh',
}

let unlistenReady: UnlistenFn | null = null

let lastLevelUpAt = 0

function currentPayload() {
  return {
    state: displayState.value,
    size: mascotSize.value,
    streams: runningCount.value,
    lite: liteMode.value,
    evolutionRealm: evolutionRealm.value,
    evolutionTier: evolutionTier.value,
    stars: mascotStars.value,
    levelUpAt: lastLevelUpAt,
  }
}

// Announce a breakthrough ONLY for genuine in-session level-ups — never for the
// initial cumulative-usage read on startup (which jumps 0 → earned level).
let levelUpArmed = false
let lastNotifiedLevel = 1
watch(
  () => mascotLevel.value.level,
  (lvl) => {
    if (!levelUpArmed) return
    if (lvl > lastNotifiedLevel) {
      const info = mascotLevel.value
      // A fresh reincarnation (new star) lands on the very first level of a cycle.
      const reincarnated = info.ascension > 0 && info.realmIndex === 0 && info.tier === 1
      const message = reincarnated
        ? t('mascot.bubble.rebirth', { stars: '⭐'.repeat(info.stars), count: info.stars })
        : t('mascot.bubble.levelUp', {
            realm: t(REALM_LABEL_KEY[info.realmId]),
            tier: info.tier,
          }) + (info.stars > 0 ? ` ${'⭐'.repeat(info.stars)}` : '')
      push(message, 'success')
      // Fire the rising-rings VFX: in-app via the shared signal, desktop pet via event.
      triggerMascotLevelUp()
      lastLevelUpAt = Date.now()
      if (enabled.value && mode.value === 'desktop') emitMascotState(currentPayload())
    }
    lastNotifiedLevel = lvl
  },
)

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
watch([displayState, runningCount, liteMode, evolutionRealm, evolutionTier], () => {
  if (enabled.value && mode.value === 'desktop') emitMascotState(currentPayload())
})

// Forward speech-bubble messages to the pet window while in desktop mode, and
// play the fox's "voice" for the bubble's variant. The sound fires from the
// main window in BOTH modes (this window is always alive and owns the settings),
// so the desktop pet stays a dumb visual renderer.
watch(
  () => bubble.current?.id,
  () => {
    if (!bubble.current || !enabled.value) return
    if (soundEnabled.value) sound.play(bubble.current.variant, bubble.current.voiceClip)
    if (mode.value === 'desktop') emitMascotBubble({ ...bubble.current })
  },
)

// Forward the "talking" flag to the pet window: audio plays here (main window),
// but the fox that should flap its mouth lives in the pet window.
watch(mascotSpeaking, (v) => {
  if (enabled.value && mode.value === 'desktop') emitMascotSpeaking(v)
})

// Resize the pet window when the size preference changes.
watch(mascotSize, async () => {
  if (enabled.value && mode.value === 'desktop') await ensureDesktopWindow()
})

onMounted(async () => {
  // Establish the earned-level baseline first so the startup 0 → earned jump
  // doesn't fire a false "breakthrough", then arm live level-up announcements.
  await levelStore.ensureLoaded()
  await nextTick()
  lastNotifiedLevel = mascotLevel.value.level
  levelUpArmed = true

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
