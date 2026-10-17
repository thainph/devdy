<script setup lang="ts">
// On-brand single-date picker (replaces native <input type="date">, whose
// popup ignores the app theme). v-model is a local `YYYY-MM-DD` string.
import { computed, ref, useAttrs } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  PopoverRoot, PopoverTrigger, PopoverPortal, PopoverContent,
  CalendarRoot, CalendarHeader, CalendarHeading, CalendarPrev, CalendarNext,
  CalendarGrid, CalendarGridHead, CalendarGridBody, CalendarGridRow,
  CalendarHeadCell, CalendarCell, CalendarCellTrigger,
} from 'radix-vue'
import type { DateValue } from '@internationalized/date'
import { CalendarDays, ChevronLeft, ChevronRight } from 'lucide-vue-next'
import { cn } from '@/lib/utils'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { dateToYmd, formatDate, intlLocale, todayDate, ymdToDate } from '@/lib/dateValue'
import type { ControlSize } from './controlStyles'
import * as s from './calendarStyles'

defineOptions({ inheritAttrs: false })

const props = withDefaults(defineProps<{
  modelValue: string
  /** Inclusive bounds, `YYYY-MM-DD`. */
  min?: string
  max?: string
  size?: ControlSize
  disabled?: boolean
  /** Prefix "Today" / "Yesterday" to the label when applicable. */
  relative?: boolean
}>(), {
  size: 'sm',
  disabled: false,
  relative: true,
})

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const { t, locale } = useI18n()
const attrs = useAttrs()
const open = ref(false)
useEscapeClose(open)

const value = computed(() => ymdToDate(props.modelValue))
const minValue = computed(() => ymdToDate(props.min))
const maxValue = computed(() => ymdToDate(props.max))

const label = computed(() => {
  const d = value.value
  if (!d) return t('common.date.pick')
  const now = todayDate()
  const sameYear = d.year === now.year
  const text = formatDate(d, locale.value, {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    ...(sameYear ? {} : { year: 'numeric' }),
  })
  if (!props.relative) return text
  if (d.compare(now) === 0) return `${t('common.date.today')} · ${text}`
  if (d.compare(now.subtract({ days: 1 })) === 0) return `${t('common.date.yesterday')} · ${text}`
  return text
})

const todayDisabled = computed(() => {
  const now = todayDate()
  return (!!maxValue.value && now.compare(maxValue.value) > 0)
    || (!!minValue.value && now.compare(minValue.value) < 0)
})

function select(d: DateValue | undefined) {
  // Clicking the selected day again yields undefined — keep the current value.
  if (!d) return
  emit('update:modelValue', dateToYmd(d))
  open.value = false
}
function pickToday() {
  select(todayDate())
}
</script>

<template>
  <PopoverRoot v-model:open="open">
    <PopoverTrigger as-child>
      <button type="button" :disabled="disabled" :class="cn(s.pickerTrigger(size), attrs.class as string | undefined)">
        <CalendarDays class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="truncate">{{ label }}</span>
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent align="start" :side-offset="6" :class="s.popoverContent">
        <CalendarRoot
          v-slot="{ weekDays, grid }"
          :model-value="value"
          :min-value="minValue"
          :max-value="maxValue"
          :locale="intlLocale(locale)"
          :week-starts-on="1"
          weekday-format="short"
          fixed-weeks
          @update:model-value="select"
        >
          <CalendarHeader :class="s.calendarHeader">
            <CalendarPrev :class="s.calendarNav"><ChevronLeft class="h-4 w-4" /></CalendarPrev>
            <CalendarHeading :class="s.calendarHeading" />
            <CalendarNext :class="s.calendarNav"><ChevronRight class="h-4 w-4" /></CalendarNext>
          </CalendarHeader>
          <CalendarGrid v-for="month in grid" :key="month.value.toString()" :class="s.calendarGrid">
            <CalendarGridHead>
              <CalendarGridRow>
                <CalendarHeadCell v-for="d in weekDays" :key="d" :class="s.calendarHeadCell">{{ d }}</CalendarHeadCell>
              </CalendarGridRow>
            </CalendarGridHead>
            <CalendarGridBody>
              <CalendarGridRow v-for="(week, i) in month.rows" :key="i">
                <CalendarCell v-for="d in week" :key="d.toString()" :date="d" :class="s.calendarCell">
                  <CalendarCellTrigger :day="d" :month="month.value" :class="s.dayCell" />
                </CalendarCell>
              </CalendarGridRow>
            </CalendarGridBody>
          </CalendarGrid>
        </CalendarRoot>
        <div class="mt-2 flex justify-end border-t border-border/60 pt-2">
          <button
            type="button"
            :disabled="todayDisabled"
            class="rounded-md px-2 py-1 text-xs text-primary transition-colors cursor-pointer hover:bg-accent disabled:opacity-40 disabled:pointer-events-none"
            @click="pickToday"
          >{{ t('common.date.today') }}</button>
        </div>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
