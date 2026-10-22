<script setup lang="ts">
// THE PR Inbox window (opened via lib/prInboxWindow, `?prInboxWindow=1`): the
// PR Inbox screen on its own OS window, for reviewing PRs beside a running
// session or on a second monitor.
//
// It renders the very same PrInboxView in `standalone` mode — no fork. That
// mode swaps what needs the main window: opening a review run / "Add to devdy"
// become requests to it (WINDOW_OPEN_RUN, PR_INBOX_WINDOW_OPEN_ROUTE — handled
// in App.vue), and run status comes from a light event feed instead of the
// liveRuns stream (see composables/useRunStatusEvents).
//
// It is its own webview with its own Pinia: the inbox, GitHub accounts and app
// settings load fresh here (PrInboxView's onMounted; App.vue's pop-out path for
// settings / theme / language). The overlay hosts the screen relies on
// (confirm dialog, toasts) are mounted here, like SlackThreadsWindow.
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ConfirmModal, ToastHost } from '@/components/ui'
import { refreshTray } from '@/lib/tray'
import PrInboxView from '@/views/PrInboxView.vue'

const { t, locale } = useI18n()

// Native title (and the menu-bar switcher's label) follows the app language,
// which App.vue applies once settings load.
watch(
  locale,
  async () => {
    const title = t('prInbox.window.title')
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
    <PrInboxView standalone class="min-h-0 flex-1" />
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
