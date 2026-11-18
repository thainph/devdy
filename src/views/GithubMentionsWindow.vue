<script setup lang="ts">
// THE GitHub Mentions window (opened via lib/githubMentionsWindow,
// `?githubMentionsWindow=1`): the GithubMentionsView in `standalone` mode, on
// its own OS window like the PR Inbox. Opening an AI-check session asks the
// main window (WINDOW_OPEN_RUN, handled in App.vue); run status comes from the
// light event feed (composables/useRunStatusEvents). Its own webview and
// Pinia: the overlay hosts the screen relies on are mounted here.
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ConfirmModal, ToastHost } from '@/components/ui'
import { refreshTray } from '@/lib/tray'
import GithubMentionsView from '@/views/GithubMentionsView.vue'

const { t, locale } = useI18n()

// Native title (and the menu-bar switcher's label) follows the app language.
watch(
  locale,
  async () => {
    const title = t('githubMentions.title')
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
    <GithubMentionsView standalone class="min-h-0 flex-1" />
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
