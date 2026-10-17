<script setup lang="ts">
// On-brand date-range picker: one trigger + a two-month range calendar.
// Bind with `v-model:from` / `v-model:to` (local `YYYY-MM-DD` strings). Both
// ends are emitted together once the second date is picked.
import { computed, ref, shallowRef, useAttrs, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  PopoverRoot, PopoverTrigger, PopoverPortal, PopoverContent,
  RangeCalendarRoot, RangeCalendarHeader, RangeCalendarHeading, RangeCalendarPrev, RangeCalendarNext,
  RangeCalendarGrid, RangeCalendarGridHead, RangeCalendarGridBody, RangeCalendarGridRow,
  RangeCalendarHeadCell, RangeCalendarCell, RangeCalendarCellTrigger,
} from 'radix-vue'
import type { DateValue } from '@internationalized/date'
import { CalendarRange, ChevronLeft, ChevronRight } from 'lucide-vue-next'
import { cn } from '@/lib/utils'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { dateToYmd, formatDateRange, intlLocale, todayDate, ymdToDate } from '@/lib/dateValue'
import type { ControlSize } from './controlStyles'
import * as s from './calendarStyles'

defineOptions({ inheritAttrs: false })

const props = withDefaults(defineProps<{
  from: string
  to: string
  /** Inclusive bounds, `YYYY-MM-DD`. */
  min?: string
  max?: string
  size?: ControlSize
  disabled?: boolean
}>(), {
  size: 'sm',
  disabled: false,
})

const emit = defineEmits<{
  'update:from': [value: string]
  'update:to': [value: string]
}>()

const { t, locale } = useI18n()
const attrs = useAttrs()
const open = ref(false)
useEscapeClose(open)

const range = computed(() => ({ start: ymdToDate(props.from), end: ymdToDate(props.to) }))
const minValue = computed(() => ymdToDate(props.min))
const maxValue = computed(() => ymdToDate(props.max))

const label = computed(() => {
  const { start, end } = range.value
  if (!start || !end) return t('common.date.pickRange')
  const now = todayDate()
  const sameYear = start.year === now.year && end.year === now.year
  return formatDateRange(start, end, locale.value, {
    day: 'numeric',
    month: 'short',
    ...(sameYear ? {} : { year: 'numeric' }),
  })
})

// Draft selection while the popover is open. RangeCalendarRoot is controlled,
// so the half-picked state (start only) must live here, not in the props.
type Draft = { start: DateValue | undefined; end: DateValue | undefined }
const draft = shallowRef<Draft>({ ...range.value })
watch(open, (v) => {
  if (v) draft.value = { ...range.value }
})

// Two months are shown; anchor so the range end sits in the RIGHT one (ranges
// are mostly in the past, so the left month is the more useful context).
const firstMonth = computed(() =>
  (range.value.end ?? todayDate()).set({ day: 1 }).subtract({ months: 1 }),
)

function onUpdate(v: Draft) {
  draft.value = v
  // Only commit once both ends are chosen.
  if (!v.start || !v.end) return
  emit('update:from', dateToYmd(v.start))
  emit('update:to', dateToYmd(v.end))
  open.value = false
}
</script>

<template>
  <PopoverRoot v-model:open="open">
    <PopoverTrigger as-child>
      <button
        type="button"
        :disabled="disabled"
        :class="cn(s.pickerTrigger(size), attrs.class as string | undefined)"
      >
        <CalendarRange class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="truncate">{{ label }}</span>
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent align="start" :side-offset="6" :class="s.popoverContent">
        <RangeCalendarRoot
          v-slot="{ weekDays, grid }"
          :model-value="draft"
          :default-placeholder="firstMonth"
          :min-value="minValue"
          :max-value="maxValue"
          :locale="intlLocale(locale)"
          :week-starts-on="1"
          :number-of-months="2"
          weekday-format="short"
          fixed-weeks
          @update:model-value="onUpdate"
        >
          <RangeCalendarHeader :class="s.calendarHeader">
            <RangeCalendarPrev :class="s.calendarNav"><ChevronLeft class="h-4 w-4" /></RangeCalendarPrev>
            <RangeCalendarHeading :class="s.calendarHeading" />
            <RangeCalendarNext :class="s.calendarNav"><ChevronRight class="h-4 w-4" /></RangeCalendarNext>
          </RangeCalendarHeader>
          <div class="flex gap-4">
            <RangeCalendarGrid v-for="month in grid" :key="month.value.toString()" :class="s.calendarGrid">
              <RangeCalendarGridHead>
                <RangeCalendarGridRow>
                  <RangeCalendarHeadCell v-for="d in weekDays" :key="d" :class="s.calendarHeadCell">{{ d }}</RangeCalendarHeadCell>
                </RangeCalendarGridRow>
              </RangeCalendarGridHead>
              <RangeCalendarGridBody>
                <RangeCalendarGridRow v-for="(week, i) in month.rows" :key="i">
                  <RangeCalendarCell v-for="d in week" :key="d.toString()" :date="d" :class="s.calendarCell">
                    <RangeCalendarCellTrigger :day="d" :month="month.value" :class="s.rangeDayCell" />
                  </RangeCalendarCell>
                </RangeCalendarGridRow>
              </RangeCalendarGridBody>
            </RangeCalendarGrid>
          </div>
        </RangeCalendarRoot>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
