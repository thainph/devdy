<script setup lang="ts">
import { computed } from 'vue'
import type { CalEvent } from '@/stores/googleCalendar'
import { startOfMonth, startOfWeek, addDays, sameDay, parseEventTime } from '@/lib/calendar'

const props = defineProps<{
  anchorDate: Date
  events: CalEvent[]
  colorFor: (accountId: string) => string
}>()

const emit = defineEmits<{ select: [ev: CalEvent] }>()

const WEEKDAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']

// 6 rows × 7 days, Monday-based, covering the anchor month plus padding days.
const days = computed(() => {
  const gridStart = startOfWeek(startOfMonth(props.anchorDate))
  return Array.from({ length: 42 }, (_, i) => addDays(gridStart, i))
})

const today = new Date()
const currentMonth = computed(() => props.anchorDate.getMonth())

// Events grouped by local day key (YYYY-M-D), sorted by start time.
const eventsByDay = computed(() => {
  const map = new Map<string, { ev: CalEvent; start: Date; allDay: boolean }[]>()
  for (const ev of props.events) {
    const { start, allDay } = parseEventTime(ev)
    const key = `${start.getFullYear()}-${start.getMonth()}-${start.getDate()}`
    if (!map.has(key)) map.set(key, [])
    map.get(key)!.push({ ev, start, allDay })
  }
  for (const list of map.values()) {
    list.sort((a, b) => Number(a.allDay) - Number(b.allDay) || a.start.getTime() - b.start.getTime())
  }
  return map
})

function eventsFor(d: Date) {
  return eventsByDay.value.get(`${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`) ?? []
}

function hhmm(d: Date) {
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Weekday header -->
    <div class="grid grid-cols-7 border-b border-border/60 shrink-0">
      <div
        v-for="w in WEEKDAYS"
        :key="w"
        class="px-2 py-1.5 text-[11px] font-medium text-muted-foreground text-center"
      >
        {{ w }}
      </div>
    </div>

    <!-- Day grid -->
    <div class="grid grid-cols-7 grid-rows-6 flex-1 min-h-0">
      <div
        v-for="(d, i) in days"
        :key="i"
        class="border-b border-r border-border/40 p-1 overflow-hidden flex flex-col min-h-0"
        :class="d.getMonth() !== currentMonth ? 'bg-muted/20' : ''"
      >
        <div class="flex items-center justify-between px-0.5">
          <span
            class="text-[11px] leading-none"
            :class="[
              d.getMonth() !== currentMonth ? 'text-muted-foreground/60' : 'text-foreground',
              sameDay(d, today)
                ? 'flex h-4 w-4 items-center justify-center rounded-full bg-primary text-primary-foreground font-semibold'
                : '',
            ]"
          >
            {{ d.getDate() }}
          </span>
        </div>

        <div class="mt-0.5 flex flex-col gap-0.5 overflow-hidden">
          <button
            v-for="{ ev, start, allDay } in eventsFor(d).slice(0, 3)"
            :key="ev.id + ev.calendar_id"
            type="button"
            class="flex items-center gap-1 rounded px-1 py-0.5 text-left text-[10px] leading-tight hover:bg-accent transition-colors"
            :title="ev.title"
            @click="emit('select', ev)"
          >
            <span
              class="h-1.5 w-1.5 shrink-0 rounded-full"
              :style="{ backgroundColor: colorFor(ev.account_id) }"
            />
            <span class="truncate">
              <span v-if="!allDay" class="text-muted-foreground">{{ hhmm(start) }} </span>{{ ev.title }}
            </span>
          </button>
          <div
            v-if="eventsFor(d).length > 3"
            class="px-1 text-[10px] text-muted-foreground"
          >
            +{{ eventsFor(d).length - 3 }} more
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
