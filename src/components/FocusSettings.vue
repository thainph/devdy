<script setup lang="ts">
// Settings → "Focus" (docs/focus-board-spec.md §5.2): working hours the board
// counts time in (deadlines without a time fall due at the end of the working
// day, BR-03), reminders (§3.4: master switch, per-kind switches, morning
// summary time), the Google Calendar used for today's free time (§3.5) and the
// estimate calibration (BR-10). The AI settings are kept as loaded and saved
// back untouched.
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Bell, CalendarClock, Gauge, Target } from 'lucide-vue-next'
import { AppSelect, Button, Card, Input } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'
import { useFocusStore } from '@/stores/focus'
import type { CalendarMeta, GoogleAccount } from '@/stores/googleCalendar'
import { DEFAULT_FOCUS_SETTINGS, NOTIFY_KINDS, notifyKindsOf, type FocusSettings } from '@/lib/focus'

const { t } = useI18n()
const { toast } = useToast()
const store = useFocusStore()

const form = ref<FocusSettings>({ ...DEFAULT_FOCUS_SETTINGS })
const loading = ref(true)
const saving = ref(false)

const DAYS = [1, 2, 3, 4, 5, 6, 7]

/** A detached copy with every reminder kind present. */
function normalised(s: FocusSettings): FocusSettings {
  return {
    ...s,
    work_days: [...s.work_days],
    calendar_ids: [...s.calendar_ids],
    notify_kinds: notifyKindsOf(s.notify_kinds),
  }
}

// ── Google accounts / calendars ──
const accounts = ref<GoogleAccount[]>([])
const calendars = ref<CalendarMeta[]>([])
const calendarsLoading = ref(false)
const calendarsError = ref<string | null>(null)

/** "" = the default Google account. */
const accountValue = computed({
  get: () => form.value.calendar_account_id ?? '',
  set: (v: string) => {
    if ((form.value.calendar_account_id ?? '') === v) return
    form.value.calendar_account_id = v || null
    form.value.calendar_ids = []
  },
})
const effectiveAccountId = computed(
  () => form.value.calendar_account_id ?? accounts.value.find((a) => a.is_default)?.id ?? accounts.value[0]?.id ?? null,
)
const accountOptions = computed(() => [
  { value: '', label: t('focus.settings.calendar.defaultAccount') },
  ...accounts.value.map((a) => ({ value: a.id, label: a.email ? `${a.label} · ${a.email}` : a.label })),
])

async function loadAccounts() {
  try {
    accounts.value = await invoke<GoogleAccount[]>('list_google_accounts')
  } catch {
    accounts.value = []
  }
}

async function loadCalendars() {
  const id = effectiveAccountId.value
  calendars.value = []
  calendarsError.value = null
  if (!id) return
  calendarsLoading.value = true
  try {
    const res = await invoke<{ calendars: CalendarMeta[]; errors: { message: string }[] }>('list_google_calendars', {
      accountIds: [id],
    })
    calendars.value = res.calendars
    calendarsError.value = res.errors[0]?.message ?? null
  } catch (e) {
    calendarsError.value = String(e)
  } finally {
    calendarsLoading.value = false
  }
}
watch(effectiveAccountId, () => void loadCalendars())

/** No calendar ticked = the primary one (backend default). */
function calendarChecked(c: CalendarMeta): boolean {
  const ids = form.value.calendar_ids
  return ids.length ? ids.includes(c.calendar_id) : c.primary
}
function toggleCalendar(c: CalendarMeta) {
  const current = new Set(
    form.value.calendar_ids.length
      ? form.value.calendar_ids
      : calendars.value.filter((x) => x.primary).map((x) => x.calendar_id),
  )
  if (current.has(c.calendar_id)) current.delete(c.calendar_id)
  else current.add(c.calendar_id)
  form.value.calendar_ids = [...current]
}

onMounted(async () => {
  await Promise.all([store.loadSettings(), loadAccounts()])
  form.value = normalised(store.settings)
  loading.value = false
  void loadCalendars()
})

function toggleDay(d: number) {
  const days = new Set(form.value.work_days)
  if (days.has(d)) days.delete(d)
  else days.add(d)
  form.value.work_days = [...days].sort((a, b) => a - b)
}

const HM = /^\d{2}:\d{2}$/
const error = computed(() => {
  const { work_start: s, work_end: e, morning_time: m } = form.value
  if (!HM.test(s) || !HM.test(e) || e <= s) return t('focus.settings.invalidHours')
  if (form.value.work_days.length === 0) return t('focus.settings.noDays')
  if (!HM.test(m)) return t('focus.settings.reminders.invalidMorning')
  return null
})

const dirty = computed(() => JSON.stringify(normalised(store.settings)) !== JSON.stringify(normalised(form.value)))

async function save() {
  if (error.value || saving.value) return
  saving.value = true
  try {
    await store.saveSettings(normalised(form.value))
    toast.success(t('focus.settings.saved'))
    // The Calendar choice may have changed: drop the cached events.
    void store.refreshCalendar()
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Card body-class="p-4 space-y-5">
    <div class="flex items-center gap-2">
      <Target class="h-4 w-4 text-muted-foreground" :stroke-width="1.75" />
      <span class="text-xs font-semibold">{{ t('focus.settings.title') }}</span>
    </div>
    <p class="text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.hint') }}</p>

    <!-- Working hours -->
    <div class="space-y-1.5">
      <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.settings.workHours') }}</label>
      <div class="flex items-center gap-2">
        <Input v-model="form.work_start" type="time" size="sm" class="w-28" :disabled="loading" :aria-label="t('focus.settings.workStart')" />
        <span class="text-muted-foreground">–</span>
        <Input v-model="form.work_end" type="time" size="sm" class="w-28" :disabled="loading" :aria-label="t('focus.settings.workEnd')" />
      </div>
    </div>

    <div class="space-y-1.5">
      <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.settings.workDays') }}</label>
      <div class="flex flex-wrap gap-1">
        <button
          v-for="d in DAYS"
          :key="d"
          type="button"
          class="h-7 w-10 cursor-pointer rounded-md border text-xs transition-colors"
          :class="form.work_days.includes(d)
            ? 'border-primary bg-primary text-primary-foreground'
            : 'border-border bg-background text-muted-foreground hover:bg-accent hover:text-foreground'"
          :aria-pressed="form.work_days.includes(d)"
          :disabled="loading"
          @click="toggleDay(d)"
        >
          {{ t(`focus.settings.days.${d}`) }}
        </button>
      </div>
    </div>

    <!-- Reminders -->
    <div class="space-y-2 border-t border-border/60 pt-4">
      <div class="flex items-center justify-between gap-3">
        <div class="min-w-0">
          <p class="flex items-center gap-1.5 text-xs font-medium">
            <Bell class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            {{ t('focus.settings.reminders.title') }}
          </p>
          <p class="mt-0.5 text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.reminders.hint') }}</p>
        </div>
        <button
          type="button"
          role="switch"
          :aria-checked="form.notify_enabled"
          :aria-label="t('focus.settings.reminders.title')"
          :disabled="loading"
          class="relative inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
          :class="form.notify_enabled ? 'bg-primary' : 'bg-muted'"
          @click="form.notify_enabled = !form.notify_enabled"
        >
          <span
            class="inline-flex h-4 w-4 transform rounded-full bg-white shadow transition-transform"
            :class="form.notify_enabled ? 'translate-x-6' : 'translate-x-1'"
          />
        </button>
      </div>
      <div v-if="form.notify_enabled" class="space-y-2 pl-5">
        <div class="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
          <label v-for="k in NOTIFY_KINDS" :key="k" class="flex cursor-pointer items-start gap-2 text-xs">
            <input v-model="form.notify_kinds[k]" type="checkbox" class="mt-0.5 accent-primary" :disabled="loading" />
            <span class="min-w-0">
              <span class="block">{{ t(`focus.settings.reminders.kinds.${k}`) }}</span>
              <span class="block text-[10px] leading-snug text-muted-foreground">{{ t(`focus.settings.reminders.kindHints.${k}`) }}</span>
            </span>
          </label>
        </div>
        <div class="flex items-center gap-2">
          <label class="text-[11px] text-muted-foreground">{{ t('focus.settings.reminders.morningTime') }}</label>
          <Input
            v-model="form.morning_time"
            type="time"
            size="sm"
            class="w-28"
            :disabled="loading || !form.notify_kinds.morning"
            :aria-label="t('focus.settings.reminders.morningTime')"
          />
        </div>
        <p class="text-[10px] leading-relaxed text-muted-foreground">{{ t('focus.settings.reminders.workHoursOnly') }}</p>
      </div>
    </div>

    <!-- Google Calendar -->
    <div class="space-y-2 border-t border-border/60 pt-4">
      <p class="flex items-center gap-1.5 text-xs font-medium">
        <CalendarClock class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
        {{ t('focus.settings.calendar.title') }}
      </p>
      <p class="text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.calendar.hint') }}</p>
      <p v-if="!loading && !accounts.length" class="text-[11px] text-muted-foreground">{{ t('focus.settings.calendar.noAccount') }}</p>
      <template v-else>
        <div class="w-80 max-w-full">
          <AppSelect v-model="accountValue" :options="accountOptions" size="sm" :disabled="loading" />
        </div>
        <p v-if="calendarsLoading" class="text-[11px] text-muted-foreground">{{ t('focus.settings.calendar.loading') }}</p>
        <p v-else-if="calendarsError" class="text-[11px] text-destructive">{{ calendarsError }}</p>
        <div v-else-if="calendars.length" class="space-y-1">
          <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.settings.calendar.busyCalendars') }}</label>
          <label v-for="c in calendars" :key="c.calendar_id" class="flex cursor-pointer items-center gap-2 text-xs">
            <input type="checkbox" class="accent-primary" :checked="calendarChecked(c)" :disabled="loading" @change="toggleCalendar(c)" />
            <span class="h-2.5 w-2.5 shrink-0 rounded-full" :style="{ backgroundColor: c.bg_color ?? 'var(--muted-foreground)' }" />
            <span class="truncate">{{ c.summary || c.calendar_id }}</span>
            <span v-if="c.primary" class="shrink-0 text-[10px] text-muted-foreground">{{ t('focus.settings.calendar.primary') }}</span>
          </label>
        </div>
      </template>
    </div>

    <!-- Calibration -->
    <div class="space-y-1 border-t border-border/60 pt-4">
      <label class="flex cursor-pointer items-start gap-2 text-xs">
        <input v-model="form.calibration_enabled" type="checkbox" class="mt-0.5 accent-primary" :disabled="loading" />
        <span class="min-w-0">
          <span class="flex items-center gap-1.5 font-medium">
            <Gauge class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            {{ t('focus.settings.calibration.title') }}
          </span>
          <span class="mt-0.5 block text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.calibration.hint') }}</span>
        </span>
      </label>
    </div>

    <p v-if="error" class="text-[11px] text-destructive">{{ error }}</p>

    <div class="flex justify-end">
      <Button size="sm" :disabled="loading || !dirty || !!error || saving" @click="save">{{ t('focus.settings.save') }}</Button>
    </div>
  </Card>
</template>
