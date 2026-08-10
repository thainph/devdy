<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { onClickOutside } from '@vueuse/core'
import {
  ChevronLeft, ChevronRight, Loader2, CalendarDays, AlertTriangle,
  ExternalLink, MapPin, Clock, CalendarRange, User, Timer,
  Video, Paperclip, AlignLeft, Settings2, Check, Languages, Bell,
} from 'lucide-vue-next'
import { openUrl } from '@tauri-apps/plugin-opener'
import { invoke } from '@/lib/tauri'
import { Button, Badge, Drawer, AppSelect } from '@/components/ui'
import { useGoogleCalendarStore, type CalEvent } from '@/stores/googleCalendar'
import { useAppSettingsStore } from '@/stores/appSettings'
import { parseEventTime, sameDay, addDays } from '@/lib/calendar'
import CalendarWeekGrid from '@/components/calendar/CalendarWeekGrid.vue'
import CalendarMonthGrid from '@/components/calendar/CalendarMonthGrid.vue'

const store = useGoogleCalendarStore()
const appSettings = useAppSettingsStore()
const router = useRouter()

onMounted(async () => {
  await store.fetchAccounts()
  if (store.accounts.length) await store.fetchEvents()
})

// App/target language for auto-translate (from Settings → Translate).
const targetLang = computed(() => appSettings.settings?.translate_target_lang || 'vi')
const LANG_NAMES: Record<string, string> = {
  vi: 'Vietnamese', en: 'English', ja: 'Japanese', zh: 'Chinese', ko: 'Korean',
  fr: 'French', de: 'German', es: 'Spanish',
}
const targetLangName = computed(() => LANG_NAMES[targetLang.value] || targetLang.value)

function toggleAutoTranslate() {
  store.setAutoTranslate(!store.autoTranslate, targetLang.value)
}

const LEAD_OPTIONS = [
  { value: '5', label: '5 min' },
  { value: '10', label: '10 min' },
  { value: '15', label: '15 min' },
  { value: '30', label: '30 min' },
  { value: '60', label: '1 hour' },
]

// Re-translate when the event set changes (fetch / navigation) while on.
watch(
  () => [store.events, store.autoTranslate] as const,
  () => { if (store.autoTranslate) void store.translateVisible(targetLang.value) },
)

// Range label for the toolbar.
const rangeLabel = computed(() => {
  const { start } = store.rangeBounds
  if (store.viewMode === 'month') {
    return store.anchorDate.toLocaleDateString('en-US', { month: 'long', year: 'numeric' })
  }
  const end = new Date(start)
  end.setDate(end.getDate() + 6)
  const fmt = (d: Date) => d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
  return `${fmt(start)} – ${fmt(end)}, ${end.getFullYear()}`
})

const visibleCount = computed(() => store.visibleEvents.length)

// ── Settings popover (view mode, navigation, account show/hide) ──
const settingsOpen = ref(false)
const settingsWrap = ref<HTMLElement | null>(null)
onClickOutside(settingsWrap, () => { settingsOpen.value = false })

// ── Event detail drawer ──
const selected = ref<CalEvent | null>(null)
const detailOpen = ref(false)
function openEvent(ev: CalEvent) {
  selected.value = ev
  detailOpen.value = true
}

// A reminder click asks us to open a specific event's detail drawer. `immediate`
// covers the case where the click routed here and this view is mounting fresh.
watch(
  () => store.requestedEvent,
  (ev) => {
    if (!ev) return
    openEvent(ev)
    void store.fetchEvents() // refresh the grid for the event's period
    store.clearRequestedEvent()
  },
  { immediate: true },
)

const selectedParsed = computed(() =>
  selected.value ? parseEventTime(selected.value) : null,
)

function relativeDay(d: Date): string {
  const now = new Date()
  if (sameDay(d, now)) return 'Today'
  if (sameDay(d, addDays(now, 1))) return 'Tomorrow'
  if (sameDay(d, addDays(now, -1))) return 'Yesterday'
  return d.toLocaleDateString('en-US', { weekday: 'long' })
}

const dateLabel = computed(() => {
  const p = selectedParsed.value
  if (!p) return ''
  const d = (x: Date) => x.toLocaleDateString('en-US', { day: 'numeric', month: 'short', year: 'numeric' })
  if (p.allDay && p.end > p.start) return `${d(p.start)} → ${d(p.end)}`
  return `${relativeDay(p.start)}, ${d(p.start)}`
})

const timeLabel = computed(() => {
  const p = selectedParsed.value
  if (!p) return ''
  if (p.allDay) return 'All day'
  const t = (x: Date) => `${String(x.getHours()).padStart(2, '0')}:${String(x.getMinutes()).padStart(2, '0')}`
  const sameDate = p.start.toDateString() === p.end.toDateString()
  return sameDate ? `${t(p.start)} – ${t(p.end)}` : `${t(p.start)} → ${p.end.toLocaleDateString('en-US', { day: 'numeric', month: 'short' })} ${t(p.end)}`
})

const durationLabel = computed(() => {
  const p = selectedParsed.value
  if (!p || p.allDay) return ''
  const mins = Math.round((p.end.getTime() - p.start.getTime()) / 60000)
  if (mins <= 0) return ''
  const h = Math.floor(mins / 60)
  const m = mins % 60
  return [h ? `${h} hr` : '', m ? `${m} min` : ''].filter(Boolean).join(' ')
})

const mapsLink = computed(() =>
  selected.value?.location
    ? `https://www.google.com/maps/search/?api=1&query=${encodeURIComponent(selected.value.location)}`
    : null,
)

// Google event descriptions can contain HTML. Strip tags to safe plain text
// (preserving line breaks / list bullets), then linkify URLs on render.
function stripHtml(html: string): string {
  return html
    .replace(/<\s*br\s*\/?\s*>/gi, '\n')
    .replace(/<\/(p|div|li|tr)\s*>/gi, '\n')
    .replace(/<\s*li[^>]*>/gi, '• ')
    .replace(/<[^>]+>/g, '')
    .replace(/&nbsp;/gi, ' ')
    .replace(/&amp;/gi, '&')
    .replace(/&lt;/gi, '<')
    .replace(/&gt;/gi, '>')
    .replace(/&quot;/gi, '"')
    .replace(/&#3[49];/g, "'")
    .replace(/\n{3,}/g, '\n\n')
    .trim()
}

// Translated description text for the open event (auto-translate on).
const translatedDesc = ref<string | null>(null)
watch(
  [detailOpen, () => store.autoTranslate, selected],
  async () => {
    translatedDesc.value = null
    if (!detailOpen.value || !store.autoTranslate || !selected.value?.description) return
    const base = stripHtml(selected.value.description)
    if (!base) return
    try {
      translatedDesc.value = await invoke<string>('translate_text', { text: base, targetLang: targetLang.value })
    } catch {
      translatedDesc.value = null
    }
  },
)

const descSegments = computed<{ text: string; href?: string }[]>(() => {
  const raw = selected.value?.description
  if (!raw) return []
  const text = translatedDesc.value ?? stripHtml(raw)
  if (!text) return []
  const parts: { text: string; href?: string }[] = []
  const re = /(https?:\/\/[^\s]+)/g
  let last = 0
  let m: RegExpExecArray | null
  while ((m = re.exec(text))) {
    if (m.index > last) parts.push({ text: text.slice(last, m.index) })
    parts.push({ text: m[1], href: m[1] })
    last = m.index + m[1].length
  }
  if (last < text.length) parts.push({ text: text.slice(last) })
  return parts
})

function attachmentName(a: { title: string | null; file_url: string | null }): string {
  return a.title || a.file_url || 'Attachment'
}

function goSettings() {
  router.push('/settings')
}

function openExternal(url: string) {
  // In the Tauri webview `window.open` is a no-op; use the opener plugin so the
  // link opens in the user's real browser.
  openUrl(url).catch(() => { /* opener unavailable (e.g. running in a plain browser) */ })
}

/** Email of the account that owns the currently selected event. */
function selectedEmail(): string | null {
  if (!selected.value) return null
  return store.accounts.find(a => a.id === selected.value!.account_id)?.email || null
}

/**
 * Append `authuser=<email>` so Google opens the link under the RIGHT account
 * when several are signed into the browser (Meet, Calendar, Drive all honour it).
 */
function withAuthUser(url: string): string {
  const email = selectedEmail()
  if (!email) return url
  try {
    const u = new URL(url)
    u.searchParams.set('authuser', email)
    return u.toString()
  } catch {
    return url
  }
}

/** Open a Google URL forcing the event's own account. */
function openForAccount(url: string) {
  openExternal(withAuthUser(url))
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header: title + current range (left), settings popover (right) -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0 gap-3">
      <div class="flex items-center gap-2 min-w-0">
        <h1 class="text-sm font-semibold shrink-0">Calendar</h1>
        <span
          v-if="store.accounts.length"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground shrink-0"
        >
          {{ visibleCount }}
        </span>
        <span v-if="store.accounts.length" class="text-sm font-medium text-foreground truncate">
          {{ rangeLabel }}
        </span>
      </div>

      <div class="flex items-center gap-2 shrink-0">
        <Loader2 v-if="store.loading" class="h-4 w-4 animate-spin text-muted-foreground" />

        <!-- Settings popover -->
        <div v-if="store.accounts.length" ref="settingsWrap" class="relative">
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Display options"
            title="Display options"
            :class="settingsOpen ? 'bg-accent text-foreground' : ''"
            @click="settingsOpen = !settingsOpen"
          >
            <Settings2 class="h-4 w-4" :stroke-width="1.75" />
          </Button>

          <div
            v-if="settingsOpen"
            class="absolute right-0 top-full z-50 mt-2 w-64 rounded-lg border border-border/60 bg-popover p-3 shadow-xl shadow-black/30"
          >
            <!-- View mode -->
            <div class="mb-3">
              <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">View</div>
              <div class="flex items-center rounded-lg border border-border/60 p-0.5 bg-muted/30" role="group" aria-label="View mode">
                <button
                  v-for="opt in [{ v: 'week', l: 'Week' }, { v: 'month', l: 'Month' }]"
                  :key="opt.v"
                  type="button"
                  class="flex-1 px-3 py-1 text-xs font-medium rounded-md cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  :class="store.viewMode === opt.v
                    ? 'bg-background text-foreground shadow-sm'
                    : 'text-muted-foreground hover:text-foreground'"
                  :aria-pressed="store.viewMode === opt.v"
                  @click="store.setViewMode(opt.v as any)"
                >{{ opt.l }}</button>
              </div>
            </div>

            <!-- Navigation -->
            <div class="mb-3">
              <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">Navigation</div>
              <div class="flex items-center gap-1">
                <Button variant="outline" size="icon-sm" aria-label="Previous" title="Previous" @click="store.step(-1)">
                  <ChevronLeft class="h-4 w-4" :stroke-width="1.75" />
                </Button>
                <Button variant="outline" size="sm" class="flex-1" @click="store.today()">Today</Button>
                <Button variant="outline" size="icon-sm" aria-label="Next" title="Next" @click="store.step(1)">
                  <ChevronRight class="h-4 w-4" :stroke-width="1.75" />
                </Button>
              </div>
            </div>

            <!-- Account show/hide -->
            <div>
              <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">Accounts</div>
              <div class="flex flex-col gap-0.5 max-h-52 overflow-auto">
                <button
                  v-for="a in store.accounts"
                  :key="a.id"
                  type="button"
                  class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm cursor-pointer hover:bg-accent transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  :class="store.visibleAccountIds.has(a.id) ? 'text-foreground' : 'text-muted-foreground'"
                  :title="a.email || a.label"
                  :aria-pressed="store.visibleAccountIds.has(a.id)"
                  @click="store.toggleAccount(a.id)"
                >
                  <span
                    class="h-2.5 w-2.5 shrink-0 rounded-full transition-opacity"
                    :style="{
                      backgroundColor: store.colorFor(a.id),
                      opacity: store.visibleAccountIds.has(a.id) ? 1 : 0.3,
                    }"
                  />
                  <span class="min-w-0 flex-1 truncate">{{ a.label }}</span>
                  <Check
                    v-if="store.visibleAccountIds.has(a.id)"
                    class="h-4 w-4 shrink-0 text-primary"
                    :stroke-width="2"
                  />
                </button>
              </div>
            </div>

            <!-- Auto-translate -->
            <div class="mt-3 pt-3 border-t border-border/60">
              <button
                type="button"
                role="switch"
                :aria-checked="store.autoTranslate"
                class="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                @click="toggleAutoTranslate"
              >
                <Languages class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <span class="min-w-0 flex-1">
                  <span class="block text-sm text-foreground">Auto-translate</span>
                  <span class="block text-[11px] text-muted-foreground truncate">
                    Translate events to {{ targetLangName }}
                  </span>
                </span>
                <Loader2 v-if="store.translating" class="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" />
                <!-- toggle track -->
                <span
                  class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
                  :class="store.autoTranslate ? 'bg-primary' : 'bg-muted'"
                >
                  <span
                    class="inline-block h-4 w-4 rounded-full bg-white shadow transition-transform"
                    :class="store.autoTranslate ? 'translate-x-4' : 'translate-x-0.5'"
                  />
                </span>
              </button>
            </div>

            <!-- Reminders -->
            <div class="mt-3 pt-3 border-t border-border/60">
              <button
                type="button"
                role="switch"
                :aria-checked="store.reminderEnabled"
                class="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                @click="store.setReminderEnabled(!store.reminderEnabled)"
              >
                <Bell class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <span class="min-w-0 flex-1">
                  <span class="block text-sm text-foreground">Event reminders</span>
                  <span class="block text-[11px] text-muted-foreground truncate">
                    Notify before an event starts
                  </span>
                </span>
                <span
                  class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
                  :class="store.reminderEnabled ? 'bg-primary' : 'bg-muted'"
                >
                  <span
                    class="inline-block h-4 w-4 rounded-full bg-white shadow transition-transform"
                    :class="store.reminderEnabled ? 'translate-x-4' : 'translate-x-0.5'"
                  />
                </span>
              </button>
              <div v-if="store.reminderEnabled" class="mt-2 flex items-center gap-2 px-1">
                <span class="text-[11px] text-muted-foreground">Remind before</span>
                <AppSelect
                  :model-value="String(store.reminderLeadMin)"
                  :options="LEAD_OPTIONS"
                  size="sm"
                  class="w-28"
                  @update:model-value="v => store.setReminderLeadMin(Number(v))"
                />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- Warnings: accounts missing calendar scope / per-account fetch errors -->
    <div
      v-if="store.accountsMissingScope.length || store.errors.length"
      class="flex items-start gap-2 px-6 py-2 border-b border-border/60 bg-amber-500/10 text-xs text-amber-600 dark:text-amber-400 shrink-0"
    >
      <AlertTriangle class="h-4 w-4 shrink-0 mt-0.5" :stroke-width="1.75" />
      <div class="flex-1">
        <template v-if="store.accountsMissingScope.length">
          Accounts missing calendar permission:
          <b>{{ store.accountsMissingScope.map(a => a.label).join(', ') }}</b>.
          Go to Settings, remove and reconnect them to grant the <code>calendar.readonly</code> scope.
        </template>
        <template v-for="err in store.errors" :key="err.account_id">
          <div>{{ err.account_label }}: {{ err.message }}</div>
        </template>
      </div>
      <Button variant="outline" size="xs" @click="goSettings">Open Settings</Button>
    </div>

    <!-- Empty state -->
    <div
      v-if="!store.accounts.length"
      class="flex-1 flex flex-col items-center justify-center gap-3 text-muted-foreground"
    >
      <CalendarDays class="h-10 w-10 opacity-40" :stroke-width="1.5" />
      <p class="text-sm">No Google account connected yet.</p>
      <Button variant="outline" size="sm" @click="goSettings">Connect in Settings</Button>
    </div>

    <!-- Grid -->
    <div v-else class="flex-1 min-h-0">
      <CalendarWeekGrid
        v-if="store.viewMode === 'week'"
        :anchor-date="store.anchorDate"
        :events="store.displayEvents"
        :color-for="store.colorFor"
        @select="openEvent"
      />
      <CalendarMonthGrid
        v-else
        :anchor-date="store.anchorDate"
        :events="store.displayEvents"
        :color-for="store.colorFor"
        @select="openEvent"
      />
    </div>

    <!-- Detail drawer -->
    <Drawer :open="detailOpen" size="sm" @close="detailOpen = false">
      <template #header>
        <div class="flex items-center gap-2 flex-1 min-w-0">
          <span
            class="h-2.5 w-2.5 shrink-0 rounded-full"
            :style="{ backgroundColor: selected ? store.colorFor(selected.account_id) : undefined }"
          />
          <h3 class="text-sm font-semibold truncate">Event details</h3>
        </div>
      </template>

      <div v-if="selected && selectedParsed" class="flex flex-col">
        <!-- Hero: colored accent bar + title + status chip -->
        <div class="relative px-5 py-4 border-b border-border/60">
          <span
            class="absolute left-0 top-4 bottom-4 w-1 rounded-full"
            :style="{ backgroundColor: store.colorFor(selected.account_id) }"
          />
          <div class="pl-3">
            <h2 class="text-lg font-semibold leading-snug break-words">{{ store.translated(selected.title) }}</h2>
            <div class="mt-2 flex flex-wrap items-center gap-1.5">
              <Badge :tone="selectedParsed.allDay ? 'info' : 'primary'" size="sm">
                {{ selectedParsed.allDay ? 'All day' : 'Timed' }}
              </Badge>
              <Badge tone="neutral" size="sm">{{ relativeDay(selectedParsed.start) }}</Badge>
            </div>
          </div>
        </div>

        <!-- Google Meet (prominent) -->
        <div v-if="selected.meet_link" class="px-3 pt-3">
          <Button variant="primary" size="sm" class="w-full" @click="openForAccount(selected.meet_link)">
            <Video class="h-4 w-4" :stroke-width="1.75" />
            Join Google Meet
          </Button>
        </div>

        <!-- Info rows -->
        <div class="flex flex-col gap-1 p-3">
          <!-- Date -->
          <div class="flex items-start gap-3 rounded-lg px-2 py-2">
            <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
              <CalendarRange class="h-4 w-4" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="text-[11px] uppercase tracking-wide text-muted-foreground">Date</div>
              <div class="text-sm text-foreground">{{ dateLabel }}</div>
            </div>
          </div>

          <!-- Time -->
          <div class="flex items-start gap-3 rounded-lg px-2 py-2">
            <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
              <Clock class="h-4 w-4" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="text-[11px] uppercase tracking-wide text-muted-foreground">Time</div>
              <div class="text-sm text-foreground flex items-center gap-2 flex-wrap">
                <span>{{ timeLabel }}</span>
                <span v-if="durationLabel" class="inline-flex items-center gap-1 text-xs text-muted-foreground">
                  <Timer class="h-3 w-3" :stroke-width="1.75" />{{ durationLabel }}
                </span>
              </div>
            </div>
          </div>

          <!-- Location -->
          <a
            v-if="selected.location && mapsLink"
            :href="mapsLink"
            class="flex items-start gap-3 rounded-lg px-2 py-2 cursor-pointer hover:bg-accent/50 transition-colors group"
            @click.prevent="openExternal(mapsLink)"
          >
            <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground group-hover:text-foreground">
              <MapPin class="h-4 w-4" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="text-[11px] uppercase tracking-wide text-muted-foreground">Location</div>
              <div class="text-sm text-primary group-hover:underline break-words">{{ store.translated(selected.location) }}</div>
            </div>
          </a>

          <!-- Account / calendar -->
          <div class="flex items-start gap-3 rounded-lg px-2 py-2">
            <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
              <User class="h-4 w-4" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="text-[11px] uppercase tracking-wide text-muted-foreground">Account</div>
              <div class="mt-0.5 flex flex-wrap items-center gap-1.5">
                <span class="inline-flex items-center gap-1.5 text-sm text-foreground">
                  <span class="h-2 w-2 rounded-full" :style="{ backgroundColor: store.colorFor(selected.account_id) }" />
                  {{ selected.account_label }}
                </span>
                <Badge tone="neutral" size="xs">{{ selected.calendar_summary }}</Badge>
              </div>
            </div>
          </div>
        </div>

        <!-- Description -->
        <div v-if="descSegments.length" class="px-5 py-4 border-t border-border/60">
          <div class="mb-2 flex items-center gap-2 text-[11px] uppercase tracking-wide text-muted-foreground">
            <AlignLeft class="h-3.5 w-3.5" :stroke-width="1.75" />
            Description
          </div>
          <div class="whitespace-pre-wrap break-words text-sm leading-relaxed text-foreground/90">
            <template v-for="(seg, i) in descSegments" :key="i">
              <a
                v-if="seg.href"
                :href="seg.href"
                class="text-primary hover:underline break-all cursor-pointer"
                @click.prevent="openExternal(seg.href)"
              >{{ seg.text }}</a>
              <span v-else>{{ seg.text }}</span>
            </template>
          </div>
        </div>

        <!-- Attachments -->
        <div v-if="selected.attachments.length" class="px-5 py-4 border-t border-border/60">
          <div class="mb-2 flex items-center gap-2 text-[11px] uppercase tracking-wide text-muted-foreground">
            <Paperclip class="h-3.5 w-3.5" :stroke-width="1.75" />
            Attachments ({{ selected.attachments.length }})
          </div>
          <div class="flex flex-col gap-1">
            <button
              v-for="(att, i) in selected.attachments"
              :key="i"
              type="button"
              class="flex items-center gap-2.5 rounded-lg border border-border/60 px-2.5 py-2 text-left cursor-pointer hover:bg-accent/50 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              :disabled="!att.file_url"
              @click="att.file_url && openForAccount(att.file_url)"
            >
              <img
                v-if="att.icon_link"
                :src="att.icon_link"
                alt=""
                class="h-5 w-5 shrink-0 rounded-sm"
              />
              <Paperclip v-else class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <span class="min-w-0 flex-1 truncate text-sm text-foreground">{{ attachmentName(att) }}</span>
              <ExternalLink v-if="att.file_url" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            </button>
          </div>
        </div>
      </div>

      <template #footer>
        <Button
          v-if="selected?.html_link"
          variant="primary"
          size="sm"
          class="w-full"
          @click="openForAccount(selected.html_link)"
        >
          <ExternalLink class="h-4 w-4" :stroke-width="1.75" />
          Open in Google Calendar
        </Button>
      </template>
    </Drawer>
  </div>
</template>
