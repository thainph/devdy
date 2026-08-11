import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref, computed } from 'vue'
import { startOfWeek, addDays, startOfMonth, endOfMonth, colorForIndex } from '@/lib/calendar'

/**
 * State for the unified Google Calendar screen. Events for ALL connected
 * accounts are fetched once per visible range (via the `list_google_calendar_events`
 * Tauri command) and filtered by `visibleAccountIds` client-side, so toggling an
 * account's visibility is instant and needs no refetch.
 *
 * Data is read-only (calendar.readonly scope); accounts connected before that
 * scope existed surface in `errors` so the view can prompt a reconnect.
 */

export interface GoogleAccount {
  id: string
  label: string
  email: string
  scope: string
  is_default: boolean
  created_at: string
  /** Computed by backend from `scope`: true when calendar write access granted. */
  calendar_writable: boolean
}

/** One calendar of a connected account (from `list_google_calendars`). */
export interface CalendarMeta {
  account_id: string
  account_label: string
  calendar_id: string
  summary: string
  bg_color: string | null
  primary: boolean
  /** Google accessRole: "owner" | "writer" | "reader" | "freeBusyReader" | null. */
  access_role: string | null
}

// ── Write payload DTOs — mirror the Rust `EventPayload` (serde camelCase). ──
export interface EventDateTimePayload {
  /** Timed event: RFC3339 local wall-clock, paired with `timeZone`. */
  dateTime?: string
  /** All-day event: YYYY-MM-DD (end is exclusive per the Google API). */
  date?: string
  /** IANA tz, timed events only. */
  timeZone?: string
}
export interface EventAttendee {
  email: string
}
export interface EventReminderOverride {
  method: 'email' | 'popup'
  minutes: number
}
export interface EventReminders {
  useDefault: boolean
  overrides?: EventReminderOverride[]
}
export interface EventConferenceData {
  createRequest: {
    requestId: string
    conferenceSolutionKey: { type: 'hangoutsMeet' }
  }
}
export interface EventPayload {
  summary?: string
  description?: string
  location?: string
  start?: EventDateTimePayload
  end?: EventDateTimePayload
  attendees?: EventAttendee[]
  reminders?: EventReminders
  /** Each element is one RRULE line, e.g. "RRULE:FREQ=WEEKLY;BYDAY=MO". */
  recurrence?: string[]
  conferenceData?: EventConferenceData
}

/**
 * Classify a backend write error string. The backend surfaces free-form
 * messages like "... failed (HTTP 403): ..." / "... (HTTP 401): ..." /
 * "Token refresh rejected (reconnect the account?): ...". A true result means
 * the account likely needs to be reconnected (missing/expired write scope).
 */
export function isAuthError(msg: string): boolean {
  return /HTTP 40[13]/.test(msg) || /reconnect the account/i.test(msg)
}

export interface CalEvent {
  id: string
  account_id: string
  account_label: string
  calendar_id: string
  calendar_summary: string
  title: string
  start: string
  end: string
  all_day: boolean
  location: string | null
  html_link: string | null
  color: string | null
  description: string | null
  meet_link: string | null
  attachments: CalAttachment[]
}

export interface CalAttachment {
  file_url: string | null
  title: string | null
  mime_type: string | null
  icon_link: string | null
}

export interface AccountError {
  account_id: string
  account_label: string
  message: string
}

interface EventsResult {
  events: CalEvent[]
  errors: AccountError[]
}

interface CalendarListResult {
  calendars: CalendarMeta[]
  errors: AccountError[]
}

export type ViewMode = 'week' | 'month'

export const useGoogleCalendarStore = defineStore('googleCalendar', () => {
  const accounts = ref<GoogleAccount[]>([])
  const calendars = ref<CalendarMeta[]>([])
  const events = ref<CalEvent[]>([])
  const errors = ref<AccountError[]>([])
  const visibleAccountIds = ref<Set<string>>(new Set())
  const viewMode = ref<ViewMode>('week')
  const anchorDate = ref<Date>(new Date())
  const loading = ref(false)
  const error = ref<string | null>(null)

  /** Stable per-account colour, keyed by position in the accounts list. */
  const colorFor = computed(() => {
    const map = new Map<string, string>()
    accounts.value.forEach((a, i) => map.set(a.id, colorForIndex(i)))
    return (accountId: string) => map.get(accountId) ?? '#6366f1'
  })

  /** Accounts missing the calendar scope (need reconnect to appear). */
  const accountsMissingScope = computed(() =>
    accounts.value.filter(a => !a.scope.includes('calendar')),
  )

  /** Accounts that lack calendar WRITE scope (can read, but not create/edit). */
  const accountsMissingWrite = computed(() =>
    accounts.value.filter(a => !a.calendar_writable),
  )

  /**
   * Calendars the user may write to (AC-12): the owning account has write scope
   * AND the calendar's accessRole is "owner" or "writer".
   */
  const writableCalendars = computed<CalendarMeta[]>(() => {
    const writableAccIds = new Set(
      accounts.value.filter(a => a.calendar_writable).map(a => a.id),
    )
    return calendars.value.filter(
      c =>
        writableAccIds.has(c.account_id) &&
        (c.access_role === 'owner' || c.access_role === 'writer'),
    )
  })

  const visibleEvents = computed(() =>
    events.value.filter(e => visibleAccountIds.value.has(e.account_id)),
  )

  // ── Auto-translate ────────────────────────────────────────────────────────
  const autoTranslate = ref(false)
  const translating = ref(false)
  // Cache keyed by ORIGINAL text → translated text (survives account toggles).
  const translations = ref<Record<string, string>>({})

  /** Visible events with title/location swapped for translations when on. */
  const displayEvents = computed<CalEvent[]>(() => {
    const on = autoTranslate.value
    const tr = translations.value
    return visibleEvents.value.map(e => {
      if (!on) return e
      return {
        ...e,
        title: tr[e.title] || e.title,
        location: e.location ? tr[e.location] || e.location : e.location,
      }
    })
  })

  function translated(text: string): string {
    return autoTranslate.value ? translations.value[text] || text : text
  }

  /** Translate every event's title + location (all accounts) in batches. */
  async function translateVisible(targetLang: string) {
    const texts = new Set<string>()
    for (const e of events.value) {
      if (e.title) texts.add(e.title)
      if (e.location) texts.add(e.location)
    }
    const pending = [...texts].filter(t => !(t in translations.value))
    if (!pending.length) return

    // Chunk by cumulative length so each call stays well under the 8k cap.
    const chunks: string[][] = []
    let cur: string[] = []
    let len = 0
    for (const t of pending) {
      if (len + t.length > 3000 && cur.length) {
        chunks.push(cur)
        cur = []
        len = 0
      }
      cur.push(t)
      len += t.length + 5
    }
    if (cur.length) chunks.push(cur)

    translating.value = true
    try {
      const next = { ...translations.value }
      // Sequential: the backend supersedes in-flight translations, so parallel
      // calls would cancel each other.
      for (const chunk of chunks) {
        const numbered = chunk
          .map((t, i) => `${i + 1}. ${t.replace(/\s+/g, ' ').trim()}`)
          .join('\n')
        let out: string
        try {
          out = await invoke<string>('translate_text', { text: numbered, targetLang })
        } catch {
          continue
        }
        const parsed: Record<number, string> = {}
        for (const line of out.split('\n')) {
          const m = line.trim().match(/^(\d+)\.\s*(.*)$/)
          if (m && m[2]) parsed[Number(m[1])] = m[2].trim()
        }
        chunk.forEach((orig, i) => {
          const tr = parsed[i + 1]
          if (tr) next[orig] = tr
        })
      }
      translations.value = next
    } finally {
      translating.value = false
    }
  }

  function setAutoTranslate(on: boolean, targetLang: string) {
    autoTranslate.value = on
    if (on) void translateVisible(targetLang)
  }

  // ── Lunar calendar overlay ────────────────────────────────────────────────
  const LS_LUNAR = 'devdy.calendar.lunarEnabled'
  const lunarEnabled = ref<boolean>(localStorage.getItem(LS_LUNAR) === '1')
  function setLunarEnabled(v: boolean) {
    lunarEnabled.value = v
    localStorage.setItem(LS_LUNAR, v ? '1' : '0')
  }

  // ── Reminders (app-wide; see CalendarReminder.vue) ────────────────────────
  const LS_ENABLED = 'devdy.calendar.reminderEnabled'
  const LS_LEAD = 'devdy.calendar.reminderLeadMin'
  const reminderEnabled = ref<boolean>(localStorage.getItem(LS_ENABLED) !== '0')
  const reminderLeadMin = ref<number>(Number(localStorage.getItem(LS_LEAD)) || 10)
  /** Events in the near-term window used to fire reminders (independent of the
   *  currently-viewed range). */
  const reminderEvents = ref<CalEvent[]>([])
  /** Bumped whenever `reminderEvents` is refreshed (by the scheduler's own fetch
   *  OR reused from a Calendar-screen fetch). The scheduler watches this to
   *  reschedule and reset its fetch countdown. */
  const reminderUpdatedAt = ref(0)
  /** Set by a reminder click → CalendarView opens this event's detail drawer. */
  const requestedEvent = ref<CalEvent | null>(null)

  function setReminderEnabled(v: boolean) {
    reminderEnabled.value = v
    localStorage.setItem(LS_ENABLED, v ? '1' : '0')
  }
  function setReminderLeadMin(m: number) {
    reminderLeadMin.value = m
    localStorage.setItem(LS_LEAD, String(m))
  }

  /** Fetch the small window of events needed to schedule reminders: from ~1 min
   *  ago (clock-skew buffer) to `now + aheadMs`. The caller sizes `aheadMs` to
   *  cover just up to the next refetch plus the lead time, so no data is wasted. */
  async function loadReminderEvents(aheadMs: number) {
    const now = Date.now()
    try {
      const res = await invoke<EventsResult>('list_google_calendar_events', {
        accountIds: null,
        timeMin: new Date(now - 60 * 1000).toISOString(),
        timeMax: new Date(now + aheadMs).toISOString(),
      })
      reminderEvents.value = res.events
      reminderUpdatedAt.value = Date.now()
    } catch {
      /* keep previous window on failure */
    }
  }

  /** Reuse a Calendar-screen fetch for reminders when its range covers "now",
   *  so we don't hit the network again just to refresh the reminder window. */
  function ingestForReminders(evs: CalEvent[], start: Date, end: Date) {
    if (!reminderEnabled.value) return
    const now = Date.now()
    if (start.getTime() <= now && end.getTime() >= now) {
      reminderEvents.value = evs
      reminderUpdatedAt.value = Date.now()
    }
  }

  function requestOpenEvent(ev: CalEvent) {
    requestedEvent.value = ev
    if (ev.start && !ev.all_day) anchorDate.value = new Date(ev.start)
  }
  function clearRequestedEvent() {
    requestedEvent.value = null
  }

  /** [start, endExclusive) covering the whole visible grid for a given anchor. */
  function boundsFor(date: Date, mode: ViewMode): { start: Date; end: Date } {
    if (mode === 'week') {
      const start = startOfWeek(date)
      return { start, end: addDays(start, 7) }
    }
    // Month grid: pad to whole weeks (Mon-start) so leading/trailing days show.
    const first = startOfMonth(date)
    const last = endOfMonth(date)
    return { start: startOfWeek(first), end: addDays(startOfWeek(last), 7) }
  }

  const rangeBounds = computed<{ start: Date; end: Date }>(() =>
    boundsFor(anchorDate.value, viewMode.value),
  )

  async function fetchAccounts() {
    try {
      accounts.value = await invoke<GoogleAccount[]>('list_google_accounts')
      // Default: every account visible.
      visibleAccountIds.value = new Set(accounts.value.map(a => a.id))
    } catch (e) {
      error.value = String(e)
    }
  }

  /** Load every calendar of every account (needed for the writable picker). */
  async function fetchCalendars() {
    try {
      const res = await invoke<CalendarListResult>('list_google_calendars', {
        accountIds: null,
      })
      calendars.value = res.calendars
    } catch (e) {
      error.value = String(e)
    }
  }

  // Per-range cache so re-visiting a week/month is instant; adjacent ranges are
  // prefetched in the background so next/prev feel snappy. Plain Map (not
  // reactive) — reads always go through events.value.
  const CACHE_TTL = 2 * 60 * 1000 // 2 min
  const cache = new Map<string, { events: CalEvent[]; errors: AccountError[]; ts: number }>()

  function keyFor(date: Date, mode: ViewMode): string {
    return `${mode}|${boundsFor(date, mode).start.toISOString()}`
  }

  async function loadRange(date: Date, mode: ViewMode): Promise<EventsResult> {
    const { start, end } = boundsFor(date, mode)
    return invoke<EventsResult>('list_google_calendar_events', {
      accountIds: null, // fetch all; filter by visibility client-side
      timeMin: start.toISOString(),
      timeMax: end.toISOString(),
    })
  }

  /** Prefetch the previous/next range so navigation is instant (fire-and-forget). */
  function prefetchNeighbors() {
    const mode = viewMode.value
    const anchors =
      mode === 'week'
        ? [addDays(anchorDate.value, -7), addDays(anchorDate.value, 7)]
        : [
            new Date(anchorDate.value.getFullYear(), anchorDate.value.getMonth() - 1, 1),
            new Date(anchorDate.value.getFullYear(), anchorDate.value.getMonth() + 1, 1),
          ]
    for (const d of anchors) {
      const key = keyFor(d, mode)
      const c = cache.get(key)
      if (c && Date.now() - c.ts < CACHE_TTL) continue
      loadRange(d, mode)
        .then(res => cache.set(key, { events: res.events, errors: res.errors, ts: Date.now() }))
        .catch(() => { /* ignore prefetch failures */ })
    }
  }

  async function fetchEvents(force = false) {
    const { start, end } = boundsFor(anchorDate.value, viewMode.value)
    const key = keyFor(anchorDate.value, viewMode.value)
    const cached = cache.get(key)
    if (!force && cached && Date.now() - cached.ts < CACHE_TTL) {
      events.value = cached.events
      errors.value = cached.errors
      ingestForReminders(cached.events, start, end)
      prefetchNeighbors()
      return
    }
    loading.value = true
    error.value = null
    try {
      const res = await loadRange(anchorDate.value, viewMode.value)
      cache.set(key, { events: res.events, errors: res.errors, ts: Date.now() })
      events.value = res.events
      errors.value = res.errors
      ingestForReminders(res.events, start, end)
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
    prefetchNeighbors()
  }

  /**
   * After a write, drop all cached ranges and re-fetch the visible range so the
   * grid reflects the change. We clear the whole cache (cheap — a handful of
   * entries) because a created/edited/recurring event can land in a neighbour
   * range that was prefetched. `fetchEvents(true)` bypasses the TTL; the trailing
   * `prefetchNeighbors()` inside it re-warms adjacent ranges freshly.
   */
  function invalidateAndRefetch() {
    cache.clear()
    return fetchEvents(true)
  }

  /**
   * Create an event, then invalidate + re-fetch (AC-11). We do NOT insert the
   * returned CalEvent directly: create/update omit calendar_summary/color, so
   * re-fetching the range is the only way to get complete grid metadata.
   */
  async function createEvent(
    accountId: string,
    calendarId: string,
    payload: EventPayload,
  ): Promise<CalEvent> {
    const ev = await invoke<CalEvent>('create_google_calendar_event', {
      accountId,
      calendarId,
      payload,
    })
    await invalidateAndRefetch()
    return ev
  }

  async function updateEvent(
    accountId: string,
    calendarId: string,
    eventId: string,
    payload: EventPayload,
  ): Promise<CalEvent> {
    const ev = await invoke<CalEvent>('update_google_calendar_event', {
      accountId,
      calendarId,
      eventId,
      payload,
    })
    await invalidateAndRefetch()
    return ev
  }

  async function deleteEvent(
    accountId: string,
    calendarId: string,
    eventId: string,
  ): Promise<void> {
    await invoke('delete_google_calendar_event', { accountId, calendarId, eventId })
    await invalidateAndRefetch()
  }

  function toggleAccount(id: string) {
    const next = new Set(visibleAccountIds.value)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    visibleAccountIds.value = next
  }

  function setViewMode(m: ViewMode) {
    if (m === viewMode.value) return
    viewMode.value = m
    fetchEvents()
  }

  function today() {
    anchorDate.value = new Date()
    fetchEvents()
  }

  function step(dir: -1 | 1) {
    if (viewMode.value === 'week') {
      anchorDate.value = addDays(anchorDate.value, dir * 7)
    } else {
      anchorDate.value = new Date(
        anchorDate.value.getFullYear(),
        anchorDate.value.getMonth() + dir,
        1,
      )
    }
    fetchEvents()
  }

  return {
    accounts,
    calendars,
    events,
    errors,
    visibleAccountIds,
    viewMode,
    anchorDate,
    loading,
    error,
    colorFor,
    accountsMissingScope,
    accountsMissingWrite,
    writableCalendars,
    visibleEvents,
    displayEvents,
    autoTranslate,
    translating,
    translations,
    translated,
    translateVisible,
    setAutoTranslate,
    lunarEnabled,
    setLunarEnabled,
    reminderEnabled,
    reminderLeadMin,
    reminderEvents,
    reminderUpdatedAt,
    requestedEvent,
    setReminderEnabled,
    setReminderLeadMin,
    loadReminderEvents,
    requestOpenEvent,
    clearRequestedEvent,
    rangeBounds,
    fetchAccounts,
    fetchCalendars,
    fetchEvents,
    createEvent,
    updateEvent,
    deleteEvent,
    toggleAccount,
    setViewMode,
    today,
    step,
  }
})
