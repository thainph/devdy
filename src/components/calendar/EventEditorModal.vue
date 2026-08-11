<script setup lang="ts">
// Create / edit a Google Calendar event. Uses the shared UI primitives
// (Modal/Input/Textarea/Button/Badge/AppSelect) and the write actions on the
// googleCalendar store. Edit mode prefills the fields carried by CalEvent
// (title, time, location, description, all-day); attendees/reminders/recurrence
// are not carried by CalEvent so they start empty and a PATCH only sends what
// the user changes.
import { computed, reactive, ref, watch } from 'vue'
import { Plus, X, Video, CalendarClock } from 'lucide-vue-next'
import { Modal, Button, Input, Textarea, Badge, AppSelect } from '@/components/ui'
import {
  useGoogleCalendarStore,
  isAuthError,
  type CalEvent,
  type EventPayload,
  type EventDateTimePayload,
  type EventReminderOverride,
} from '@/stores/googleCalendar'
import { useToast } from '@/composables/useToast'
import { parseEventTime, fmtLocal, addDays } from '@/lib/calendar'

const props = defineProps<{
  open: boolean
  mode: 'create' | 'edit'
  /** Required in edit mode — source for the prefill. */
  event?: CalEvent | null
  /** Prefill start when creating from a clicked empty grid cell (AC-17). */
  defaultStart?: Date | null
}>()

const emit = defineEmits<{
  close: []
  saved: [ev: CalEvent]
}>()

const store = useGoogleCalendarStore()
const { toast } = useToast()

interface FormState {
  title: string
  accountId: string
  calendarId: string
  allDay: boolean
  startDate: string
  startTime: string
  endDate: string
  endTime: string
  location: string
  description: string
  attendees: string[]
  attendeeDraft: string
  reminders: EventReminderOverride[]
  recurrencePreset: 'none' | 'daily' | 'weekly' | 'monthly' | 'custom'
  customRRule: string
  addMeet: boolean
}

function hhmm(d: Date): string {
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

function emptyForm(): FormState {
  const now = new Date()
  now.setMinutes(0, 0, 0)
  const later = new Date(now.getTime() + 60 * 60 * 1000)
  return {
    title: '',
    accountId: '',
    calendarId: '',
    allDay: false,
    startDate: fmtLocal(now),
    startTime: hhmm(now),
    endDate: fmtLocal(later),
    endTime: hhmm(later),
    location: '',
    description: '',
    attendees: [],
    attendeeDraft: '',
    reminders: [],
    recurrencePreset: 'none',
    customRRule: '',
    addMeet: false,
  }
}

const form = reactive<FormState>(emptyForm())
const submitting = ref(false)

// ── Pickers (writable calendars only) ─────────────────────────────────────
const writableAccountOptions = computed(() => {
  const ids = new Set(store.writableCalendars.map(c => c.account_id))
  return store.accounts
    .filter(a => ids.has(a.id))
    .map(a => ({ value: a.id, label: a.label }))
})

const calendarOptions = computed(() =>
  store.writableCalendars
    .filter(c => c.account_id === form.accountId)
    .map(c => ({ value: c.calendar_id, label: c.summary || c.calendar_id })),
)

function defaultCalendarFor(accountId: string): string {
  const cals = store.writableCalendars.filter(c => c.account_id === accountId)
  const primary = cals.find(c => c.primary)
  return primary?.calendar_id ?? cals[0]?.calendar_id ?? ''
}

// When the account changes, reset the calendar to that account's primary.
watch(
  () => form.accountId,
  (id) => {
    if (!id) return
    const stillValid = store.writableCalendars.some(
      c => c.account_id === id && c.calendar_id === form.calendarId,
    )
    if (!stillValid) form.calendarId = defaultCalendarFor(id)
  },
)

// ── Reset / prefill whenever the modal opens ──────────────────────────────
function reset() {
  Object.assign(form, emptyForm())

  if (props.mode === 'edit' && props.event) {
    const ev = props.event
    const p = parseEventTime(ev)
    form.title = ev.title === '(no title)' ? '' : ev.title
    form.accountId = ev.account_id
    form.calendarId = ev.calendar_id
    form.allDay = p.allDay
    form.startDate = fmtLocal(p.start)
    form.endDate = fmtLocal(p.end)
    if (!p.allDay) {
      form.startTime = hhmm(p.start)
      form.endTime = hhmm(p.end)
    }
    form.location = ev.location ?? ''
    form.description = ev.description ?? ''
    return
  }

  // Create mode — seed the first writable account/calendar.
  const firstAcc = writableAccountOptions.value[0]?.value ?? ''
  form.accountId = firstAcc
  form.calendarId = defaultCalendarFor(firstAcc)

  if (props.defaultStart) {
    const start = props.defaultStart
    const end = new Date(start.getTime() + 60 * 60 * 1000)
    form.startDate = fmtLocal(start)
    form.startTime = hhmm(start)
    form.endDate = fmtLocal(end)
    form.endTime = hhmm(end)
  }
}

watch(
  () => props.open,
  (open) => {
    if (open) reset()
  },
)

// ── Attendees (email chips) ───────────────────────────────────────────────
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/

function addAttendee() {
  const email = form.attendeeDraft.trim()
  if (!email) return
  if (!EMAIL_RE.test(email)) {
    toast.error('Email không hợp lệ.')
    return
  }
  if (!form.attendees.includes(email)) form.attendees.push(email)
  form.attendeeDraft = ''
}

function removeAttendee(email: string) {
  form.attendees = form.attendees.filter(e => e !== email)
}

// ── Reminders ─────────────────────────────────────────────────────────────
const REMINDER_METHODS = [
  { value: 'popup', label: 'Thông báo' },
  { value: 'email', label: 'Email' },
]

function addReminder() {
  form.reminders.push({ method: 'popup', minutes: 10 })
}

function removeReminder(i: number) {
  form.reminders.splice(i, 1)
}

// ── Recurrence ────────────────────────────────────────────────────────────
const RECURRENCE_OPTIONS = [
  { value: 'none', label: 'Không lặp' },
  { value: 'daily', label: 'Hằng ngày' },
  { value: 'weekly', label: 'Hằng tuần' },
  { value: 'monthly', label: 'Hằng tháng' },
  { value: 'custom', label: 'Tùy chỉnh (RRULE)' },
]

// ── Validation (AC-15) ────────────────────────────────────────────────────
const validationErrors = computed<string[]>(() => {
  const errs: string[] = []
  if (!form.calendarId) errs.push('Chọn lịch để lưu event.')

  if (form.allDay) {
    if (form.endDate < form.startDate) {
      errs.push('Ngày kết thúc phải bằng hoặc sau ngày bắt đầu.')
    }
  } else {
    const start = new Date(`${form.startDate}T${form.startTime}:00`)
    const end = new Date(`${form.endDate}T${form.endTime}:00`)
    if (!(end.getTime() > start.getTime())) {
      errs.push('Thời gian kết thúc phải sau thời gian bắt đầu.')
    }
  }

  for (const email of form.attendees) {
    if (!EMAIL_RE.test(email)) {
      errs.push(`Email không hợp lệ: ${email}`)
      break
    }
  }

  if (form.recurrencePreset === 'custom' && !/^RRULE:/i.test(form.customRRule.trim())) {
    errs.push("RRULE phải bắt đầu bằng 'RRULE:'.")
  }

  return errs
})

const firstError = computed(() => validationErrors.value[0] ?? '')
const canSubmit = computed(() => validationErrors.value.length === 0 && !submitting.value)

// ── Build the EventPayload ────────────────────────────────────────────────
const LOCAL_TZ = Intl.DateTimeFormat().resolvedOptions().timeZone

function buildDateTime(dateStr: string, timeStr: string): EventDateTimePayload {
  if (form.allDay) return { date: dateStr }
  return { dateTime: `${dateStr}T${timeStr}:00`, timeZone: LOCAL_TZ }
}

function buildEndDateTime(): EventDateTimePayload {
  if (form.allDay) {
    // Google end date is EXCLUSIVE → push the picked day forward by one.
    const exclusive = addDays(new Date(`${form.endDate}T00:00:00`), 1)
    return { date: fmtLocal(exclusive) }
  }
  return { dateTime: `${form.endDate}T${form.endTime}:00`, timeZone: LOCAL_TZ }
}

function recurrenceLines(): string[] | undefined {
  switch (form.recurrencePreset) {
    case 'daily':
      return ['RRULE:FREQ=DAILY']
    case 'weekly':
      return ['RRULE:FREQ=WEEKLY']
    case 'monthly':
      return ['RRULE:FREQ=MONTHLY']
    case 'custom':
      return [form.customRRule.trim()]
    default:
      return undefined
  }
}

function buildPayload(): EventPayload {
  const payload: EventPayload = {
    summary: form.title,
    location: form.location,
    description: form.description,
    start: buildDateTime(form.startDate, form.startTime),
    end: buildEndDateTime(),
  }

  if (form.attendees.length) {
    payload.attendees = form.attendees.map(email => ({ email }))
  }

  if (form.reminders.length) {
    payload.reminders = { useDefault: false, overrides: form.reminders.map(r => ({ ...r })) }
  }

  const rec = recurrenceLines()
  if (rec) payload.recurrence = rec

  if (form.addMeet) {
    payload.conferenceData = {
      createRequest: {
        requestId: crypto.randomUUID(),
        conferenceSolutionKey: { type: 'hangoutsMeet' },
      },
    }
  }

  return payload
}

// ── Submit ────────────────────────────────────────────────────────────────
async function submit() {
  if (!canSubmit.value) return
  submitting.value = true
  try {
    const payload = buildPayload()
    const ev =
      props.mode === 'create'
        ? await store.createEvent(form.accountId, form.calendarId, payload)
        : await store.updateEvent(
            props.event!.account_id,
            props.event!.calendar_id,
            props.event!.id,
            payload,
          )
    toast.success(props.mode === 'create' ? 'Đã tạo event.' : 'Đã cập nhật event.')
    emit('saved', ev)
    emit('close')
  } catch (e) {
    const msg = String(e)
    toast.error(
      isAuthError(msg)
        ? 'Không đủ quyền ghi lịch. Hãy kết nối lại tài khoản trong Settings.'
        : `Lưu thất bại: ${msg}`,
    )
  } finally {
    submitting.value = false
  }
}

const modalTitle = computed(() => (props.mode === 'create' ? 'Tạo event' : 'Sửa event'))
const isEdit = computed(() => props.mode === 'edit')
</script>

<template>
  <Modal :open="open" :title="modalTitle" size="lg" scroll-body @close="emit('close')">
    <div class="flex flex-col gap-3 p-4">
      <!-- Title -->
      <div class="flex flex-col gap-1.5">
        <label class="text-xs font-medium text-muted-foreground">Tiêu đề</label>
        <Input v-model="form.title" size="sm" placeholder="Tên event" />
      </div>

      <!-- Account + calendar -->
      <div class="grid grid-cols-2 gap-3">
        <div class="flex flex-col gap-1.5 min-w-0">
          <label class="text-xs font-medium text-muted-foreground">Tài khoản</label>
          <AppSelect
            v-model="form.accountId"
            :options="writableAccountOptions"
            size="sm"
            placeholder="Chọn tài khoản"
            :disabled="isEdit"
          />
        </div>
        <div class="flex flex-col gap-1.5 min-w-0">
          <label class="text-xs font-medium text-muted-foreground">Lịch</label>
          <AppSelect
            v-model="form.calendarId"
            :options="calendarOptions"
            size="sm"
            placeholder="Chọn lịch"
            :disabled="isEdit"
          />
        </div>
      </div>

      <!-- All-day toggle -->
      <button
        type="button"
        role="switch"
        :aria-checked="form.allDay"
        class="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        @click="form.allDay = !form.allDay"
      >
        <CalendarClock class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="min-w-0 flex-1 text-sm text-foreground">Cả ngày</span>
        <span
          class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
          :class="form.allDay ? 'bg-primary' : 'bg-muted'"
        >
          <span
            class="inline-block h-4 w-4 rounded-full bg-white shadow transition-transform"
            :class="form.allDay ? 'translate-x-4' : 'translate-x-0.5'"
          />
        </span>
      </button>

      <!-- Start / end -->
      <div class="grid grid-cols-2 gap-3">
        <div class="flex flex-col gap-1.5">
          <label class="text-xs font-medium text-muted-foreground">Bắt đầu</label>
          <div class="flex gap-2">
            <Input v-model="form.startDate" type="date" size="sm" class="flex-1" />
            <Input v-if="!form.allDay" v-model="form.startTime" type="time" size="sm" class="w-28" />
          </div>
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-xs font-medium text-muted-foreground">Kết thúc</label>
          <div class="flex gap-2">
            <Input v-model="form.endDate" type="date" size="sm" class="flex-1" />
            <Input v-if="!form.allDay" v-model="form.endTime" type="time" size="sm" class="w-28" />
          </div>
        </div>
      </div>

      <!-- Location -->
      <div class="flex flex-col gap-1.5">
        <label class="text-xs font-medium text-muted-foreground">Địa điểm</label>
        <Input v-model="form.location" size="sm" placeholder="Địa điểm (tùy chọn)" />
      </div>

      <!-- Description -->
      <div class="flex flex-col gap-1.5">
        <label class="text-xs font-medium text-muted-foreground">Mô tả</label>
        <Textarea v-model="form.description" size="sm" rows="3" placeholder="Mô tả (tùy chọn)" />
      </div>

      <!-- Attendees -->
      <div class="flex flex-col gap-1.5">
        <label class="text-xs font-medium text-muted-foreground">Người tham dự</label>
        <p v-if="isEdit" class="text-[11px] text-muted-foreground">
          Để trống nếu không muốn thay đổi danh sách hiện có.
        </p>
        <div v-if="form.attendees.length" class="flex flex-wrap gap-1.5">
          <Badge v-for="email in form.attendees" :key="email" tone="neutral" size="sm">
            {{ email }}
            <button
              type="button"
              class="ml-0.5 cursor-pointer hover:text-red-300"
              aria-label="Xóa"
              @click="removeAttendee(email)"
            >
              <X class="h-3 w-3" :stroke-width="2" />
            </button>
          </Badge>
        </div>
        <div class="flex gap-2">
          <Input
            v-model="form.attendeeDraft"
            size="sm"
            type="email"
            placeholder="email@example.com"
            class="flex-1"
            @keydown.enter.prevent="addAttendee"
            @blur="addAttendee"
          />
          <Button variant="outline" size="sm" @click="addAttendee">
            <Plus class="h-4 w-4" :stroke-width="1.75" /> Thêm
          </Button>
        </div>
      </div>

      <!-- Reminders -->
      <div class="flex flex-col gap-1.5">
        <label class="text-xs font-medium text-muted-foreground">Nhắc nhở</label>
        <p v-if="isEdit" class="text-[11px] text-muted-foreground">
          Để trống nếu không muốn thay đổi nhắc nhở hiện có.
        </p>
        <div v-for="(r, i) in form.reminders" :key="i" class="flex items-center gap-2">
          <AppSelect
            :model-value="r.method"
            :options="REMINDER_METHODS"
            size="sm"
            class="w-36"
            @update:model-value="v => (r.method = v as 'popup' | 'email')"
          />
          <Input
            :model-value="String(r.minutes)"
            type="number"
            size="sm"
            class="w-24"
            @update:model-value="v => (r.minutes = Number(v))"
          />
          <span class="text-xs text-muted-foreground">phút trước</span>
          <Button variant="destructive-ghost" size="icon-sm" aria-label="Xóa" @click="removeReminder(i)">
            <X class="h-4 w-4" :stroke-width="1.75" />
          </Button>
        </div>
        <div>
          <Button variant="outline" size="sm" @click="addReminder">
            <Plus class="h-4 w-4" :stroke-width="1.75" /> Thêm nhắc nhở
          </Button>
        </div>
      </div>

      <!-- Recurrence -->
      <div class="flex flex-col gap-1.5">
        <label class="text-xs font-medium text-muted-foreground">Lặp lại</label>
        <p v-if="isEdit" class="text-[11px] text-muted-foreground">
          Để "Không lặp" nếu không muốn thay đổi lặp lại hiện có.
        </p>
        <AppSelect
          v-model="form.recurrencePreset"
          :options="RECURRENCE_OPTIONS"
          size="sm"
          class="w-full"
        />
        <Input
          v-if="form.recurrencePreset === 'custom'"
          v-model="form.customRRule"
          size="sm"
          placeholder="RRULE:FREQ=WEEKLY;BYDAY=MO,WE"
        />
      </div>

      <!-- Google Meet -->
      <button
        type="button"
        role="switch"
        :aria-checked="form.addMeet"
        class="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        @click="form.addMeet = !form.addMeet"
      >
        <Video class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="min-w-0 flex-1 text-sm text-foreground">Thêm Google Meet</span>
        <span
          class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
          :class="form.addMeet ? 'bg-primary' : 'bg-muted'"
        >
          <span
            class="inline-block h-4 w-4 rounded-full bg-white shadow transition-transform"
            :class="form.addMeet ? 'translate-x-4' : 'translate-x-0.5'"
          />
        </span>
      </button>
    </div>

    <template #footer>
      <span v-if="firstError" class="mr-auto text-xs text-red-500 dark:text-red-400">{{ firstError }}</span>
      <Button variant="outline" size="sm" :disabled="submitting" @click="emit('close')">Hủy</Button>
      <Button variant="primary" size="sm" :disabled="!canSubmit" @click="submit">
        {{ mode === 'create' ? 'Tạo' : 'Lưu' }}
      </Button>
    </template>
  </Modal>
</template>
