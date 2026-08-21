<script setup lang="ts">
import { onMounted, onBeforeUnmount, watch } from 'vue'
import { useRouter } from 'vue-router'
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification'
import { invoke } from '@/lib/tauri'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useGoogleCalendarStore, type CalEvent } from '@/stores/googleCalendar'
import { parseEventTime } from '@/lib/calendar'
import { useMascotBubble } from '@/composables/useMascotBubble'

/**
 * Headless, app-wide calendar reminder scheduler. Renders nothing. It keeps a
 * near-term window of events refreshed (independent of whether the Calendar
 * screen is open) and fires a native OS notification when a timed event is about
 * to start (within the configured lead time). Clicking the notification focuses
 * the window, routes to the Calendar screen, and opens that event's detail
 * drawer (via the store's `requestedEvent`).
 */

const store = useGoogleCalendarStore()
const router = useRouter()
const { push: pushBubble } = useMascotBubble()

const FETCH_INTERVAL_MS = 15 * 60 * 1000 // re-pull the reminder window every 15 min
const SAFETY_MARGIN_MS = 5 * 60 * 1000 // extra buffer so a late/failed fetch can't miss a reminder

/** How far ahead each fetch must look: enough to cover up to the next refetch
 *  plus the lead time (+ margin), so every reminder due before the next fetch is
 *  already loaded. Kept small on purpose — the huge window was redundant given
 *  the periodic refetch. */
function lookaheadMs(): number {
  return Math.max(0, store.reminderLeadMin) * 60 * 1000 + FETCH_INTERVAL_MS + SAFETY_MARGIN_MS
}

// Resettable timer for the next network refetch. Reset whenever the reminder
// data is refreshed from ANY source (our own fetch OR a Calendar-screen fetch),
// so a view refetch defers the redundant background fetch.
let fetchTimer: number | null = null
// Event-driven: instead of polling every N seconds, we setTimeout to the exact
// next due moment. No periodic wake-ups → negligible idle cost.
let dueTimer: number | null = null
let unlistenClick: UnlistenFn | null = null
let permissionGranted = false
// Keyed by `${id}|${start}` so each occurrence alerts at most once per session.
const notified = new Set<string>()

function fmtTime(d: Date): string {
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

/** (Re)arm the background refetch timer. */
function scheduleFetch() {
  if (fetchTimer) window.clearTimeout(fetchTimer)
  fetchTimer = window.setTimeout(() => void refresh(), FETCH_INTERVAL_MS)
}

async function refresh() {
  if (!store.reminderEnabled) return
  if (!store.accounts.length) await store.fetchAccounts()
  if (!store.accounts.length) return
  // loadReminderEvents bumps reminderUpdatedAt on success → the watcher below
  // reschedules notifications and the fetch timer. Always (re)arm the fetch
  // timer here too so a failed fetch still retries.
  await store.loadReminderEvents(lookaheadMs())
  scheduleFetch()
}

/** Fire notifications for every event currently within the lead window. */
function fireDue() {
  if (!store.reminderEnabled || !permissionGranted) return
  const now = Date.now()
  const leadMs = Math.max(0, store.reminderLeadMin) * 60 * 1000
  for (const ev of store.reminderEvents) {
    if (ev.all_day) continue // all-day events have no precise trigger time
    const startMs = parseEventTime(ev).start.getTime()
    if (startMs < now || startMs > now + leadMs) continue
    const key = `${ev.id}|${ev.start}`
    if (notified.has(key)) continue
    notified.add(key)

    const start = parseEventTime(ev).start
    const mins = Math.max(0, Math.round((startMs - now) / 60000))
    const when = mins <= 0 ? 'starting now' : `starts in ${mins} min`
    const body = `${fmtTime(start)} · ${when}${ev.account_label ? ` · ${ev.account_label}` : ''}`
    invoke('show_calendar_reminder', {
      title: ev.title || 'Event',
      body,
      eventId: ev.id,
    }).catch(() => { /* ignore — best-effort */ })

    // Also surface it in the DY mascot's speech bubble.
    pushBubble(`${ev.title || 'Event'} · ${body}`, 'info', 8000)
  }
}

/** Delay (ms) until the soonest not-yet-notified event enters its lead window,
 *  or null when there's nothing left to remind. */
function nextDelayMs(): number | null {
  if (!store.reminderEnabled || !permissionGranted) return null
  const now = Date.now()
  const leadMs = Math.max(0, store.reminderLeadMin) * 60 * 1000
  let soonest = Infinity
  for (const ev of store.reminderEvents) {
    if (ev.all_day) continue
    const startMs = parseEventTime(ev).start.getTime()
    if (startMs < now) continue // already started
    const key = `${ev.id}|${ev.start}`
    if (notified.has(key)) continue
    // Trigger = lead window opens; if already inside it, due immediately.
    const trigger = Math.max(now, startMs - leadMs)
    if (trigger < soonest) soonest = trigger
  }
  return soonest === Infinity ? null : Math.max(0, soonest - now)
}

/** (Re)arm a single timer for the exact next due moment. */
function scheduleNext() {
  if (dueTimer) {
    window.clearTimeout(dueTimer)
    dueTimer = null
  }
  const delay = nextDelayMs()
  if (delay === null) return
  // Cap so a long wait still re-evaluates periodically (the 5-min refresh also
  // reschedules when data changes).
  dueTimer = window.setTimeout(() => {
    fireDue()
    scheduleNext()
  }, Math.min(delay, FETCH_INTERVAL_MS))
}

onMounted(async () => {
  try {
    permissionGranted = await isPermissionGranted()
    if (!permissionGranted) permissionGranted = (await requestPermission()) === 'granted'
  } catch {
    permissionGranted = false
  }

  // Native click routing (the plugin's onAction never fires on desktop; the Rust
  // side emits this event carrying the event id).
  try {
    unlistenClick = await listen<{ eventId?: string }>('calendar-reminder-clicked', (e) => {
      const id = e.payload?.eventId
      getCurrentWindow().setFocus().catch(() => {})
      if (!id) return
      const ev: CalEvent | undefined = store.reminderEvents.find(x => x.id === id)
      router.push('/calendar').catch(() => {})
      if (ev) store.requestOpenEvent(ev)
    })
  } catch {
    unlistenClick = null
  }

  await refresh()
})

onBeforeUnmount(() => {
  if (fetchTimer) window.clearTimeout(fetchTimer)
  if (dueTimer) window.clearTimeout(dueTimer)
  unlistenClick?.()
})

// Whenever the reminder data is refreshed — by our own fetch OR reused from a
// Calendar-screen fetch — reschedule notifications AND reset the fetch countdown
// so a fresh view fetch defers the next redundant background fetch.
watch(
  () => store.reminderUpdatedAt,
  () => {
    scheduleNext()
    scheduleFetch()
  },
)

// Toggling reminders on → (re)load; off → stop all timers.
watch(
  () => store.reminderEnabled,
  (on) => {
    if (on) {
      void refresh()
    } else {
      if (fetchTimer) window.clearTimeout(fetchTimer)
      if (dueTimer) window.clearTimeout(dueTimer)
    }
  },
)

// Lead time affects the fetch window and trigger points → refetch + reschedule.
watch(() => store.reminderLeadMin, () => void refresh())
</script>

<template>
  <span hidden aria-hidden="true" />
</template>
