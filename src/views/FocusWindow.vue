<script setup lang="ts">
// THE Focus window (opened via lib/focusWindow, `?focusWindow=1`): the Focus
// board on its own OS window, beside a running session or on a second monitor.
//
// It renders the very same FocusView — no fork; nothing in it needs the main
// window's router. It is its own webview with its own Pinia: the focus store
// loads fresh here and stays live through `focus:changed` / `item:changed`. The
// overlay hosts the screen relies on (confirm dialog, toasts) are mounted here,
// like PrInboxWindow.
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ConfirmModal, ToastHost } from '@/components/ui'
import { refreshTray } from '@/lib/tray'
import FocusView from '@/views/FocusView.vue'

const { t, locale } = useI18n()

// Native title (and the menu-bar switcher's label) follows the app language,
// which App.vue applies once settings load.
watch(
  locale,
  async () => {
    const title = t('focus.window.title')
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
</script>

<template>
  <div class="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
    <FocusView class="min-h-0 flex-1" />
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
