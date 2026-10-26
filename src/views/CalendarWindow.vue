<script setup lang="ts">
// THE Calendar window (opened via lib/calendarWindow, `?calendarWindow=1`): the
// Calendar screen on its own OS window, beside a running session or on a
// second monitor.
//
// It renders the very same CalendarView in `standalone` mode — no fork. That
// mode swaps what needs the main window: "Reconnect" becomes a request to it
// (CALENDAR_WINDOW_OPEN_ROUTE, handled in App.vue). Reminders stay owned by the
// main window (CalendarReminder); a reminder click opens this window and hands
// it the event (CALENDAR_WINDOW_OPEN_EVENT).
//
// It is its own webview with its own Pinia: accounts and events load fresh here
// (CalendarView's onMounted). The overlay hosts the screen relies on (confirm
// dialog, toasts) are mounted here, like PrInboxWindow.
import { onBeforeUnmount, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { ConfirmModal, ToastHost } from '@/components/ui'
import { refreshTray } from '@/lib/tray'
import { CALENDAR_WINDOW_OPEN_EVENT, CALENDAR_WINDOW_READY } from '@/lib/calendarWindow'
import { useGoogleCalendarStore, type CalEvent } from '@/stores/googleCalendar'
import CalendarView from '@/views/CalendarView.vue'

const { t, locale } = useI18n()
const store = useGoogleCalendarStore()

// Native title (and the menu-bar switcher's label) follows the app language,
// which App.vue applies once settings load.
watch(
  locale,
  async () => {
    const title = t('calendar.window.title')
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

let unlistenOpenEvent: UnlistenFn | null = null
onMounted(async () => {
  try {
    unlistenOpenEvent = await listen<CalEvent>(CALENDAR_WINDOW_OPEN_EVENT, (e) => {
      if (e.payload?.id) store.requestOpenEvent(e.payload)
    })
    await emit(CALENDAR_WINDOW_READY)
  } catch {
    /* outside the Tauri shell */
  }
})
onBeforeUnmount(() => unlistenOpenEvent?.())
</script>

<template>
  <div class="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
    <CalendarView standalone class="min-h-0 flex-1" />
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
