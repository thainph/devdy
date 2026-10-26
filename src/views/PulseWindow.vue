<script setup lang="ts">
// THE Pulse window (opened via lib/pulseWindow, `?pulseWindow=1`): the Project
// Pulse screen on its own OS window, beside a running session or on a second
// monitor.
//
// It renders the very same PulseView — no fork; nothing in it needs the main
// window's router. It is its own webview with its own Pinia: the pulse store
// loads fresh here and stays live through the backend's pulse events, as does
// the main window's (sidebar badge). The overlay hosts the screen relies on
// (confirm dialog, toasts) are mounted here, like PrInboxWindow.
import { onBeforeUnmount, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ConfirmModal, ToastHost } from '@/components/ui'
import { refreshTray } from '@/lib/tray'
import { usePulseStore } from '@/stores/pulse'
import PulseView from '@/views/PulseView.vue'

const { t, locale } = useI18n()
const store = usePulseStore()

// Native title (and the menu-bar switcher's label) follows the app language,
// which App.vue applies once settings load.
watch(
  locale,
  async () => {
    const title = t('pulse.window.title')
    document.title = title
    try {
      await getCurrentWindow().setTitle(title)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

// PulseView flags the screen open while mounted (BR-11), but closing the OS
// window tears the webview down without unmounting it — clear the flag first.
let unlistenClose: (() => void) | null = null
onMounted(async () => {
  try {
    unlistenClose = await getCurrentWindow().onCloseRequested(async () => {
      store.setScreenOpen(false)
    })
  } catch {
    /* outside the Tauri shell */
  }
})
onBeforeUnmount(() => unlistenClose?.())
</script>

<template>
  <div class="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
    <PulseView class="min-h-0 flex-1" />
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
