<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import type { CalEvent } from '@/stores/googleCalendar'
import { startOfWeek, addDays, sameDay, minutesOfDay, parseEventTime } from '@/lib/calendar'

const props = defineProps<{
  anchorDate: Date
  events: CalEvent[]
  colorFor: (accountId: string) => string
}>()

const emit = defineEmits<{ select: [ev: CalEvent] }>()

const HOUR_HEIGHT = 44 // px per hour
const HOURS = Array.from({ length: 24 }, (_, i) => i)
const WEEKDAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']

const weekDays = computed(() => {
  const start = startOfWeek(props.anchorDate)
  return Array.from({ length: 7 }, (_, i) => addDays(start, i))
})

const today = new Date()

interface Parsed {
  ev: CalEvent
  start: Date
  end: Date
  allDay: boolean
}

const parsed = computed<Parsed[]>(() =>
  props.events.map(ev => ({ ev, ...parseEventTime(ev) })),
)

// ── All-day events: shown in the header strip, on each covered day of the week ──
function allDayFor(day: Date) {
  return parsed.value.filter(
    p => p.allDay && day >= new Date(p.start.getFullYear(), p.start.getMonth(), p.start.getDate()) && day <= p.end,
  )
}

// ── Timed events: positioned blocks with overlap-aware lane packing ──
interface Block {
  ev: CalEvent
  start: Date
  top: number
  height: number
  leftPct: number
  widthPct: number
}

function timedBlocks(day: Date): Block[] {
  const items = parsed.value
    .filter(p => !p.allDay && sameDay(p.start, day))
    .sort((a, b) => a.start.getTime() - b.start.getTime())
  if (!items.length) return []

  // Split into clusters of transitively-overlapping events.
  const blocks: Block[] = []
  let cluster: Parsed[] = []
  let clusterEnd = 0

  const flush = () => {
    if (!cluster.length) return
    // Assign each event to the first free lane.
    const laneEnds: number[] = []
    const laneOf = new Map<Parsed, number>()
    for (const it of cluster) {
      const s = it.start.getTime()
      let lane = laneEnds.findIndex(end => end <= s)
      if (lane === -1) {
        lane = laneEnds.length
        laneEnds.push(0)
      }
      laneEnds[lane] = it.end.getTime()
      laneOf.set(it, lane)
    }
    const lanes = laneEnds.length
    for (const it of cluster) {
      const startMin = minutesOfDay(it.start)
      const endMin = it.end.getTime() > it.start.getTime()
        ? Math.min(24 * 60, minutesOfDay(it.start) + (it.end.getTime() - it.start.getTime()) / 60000)
        : startMin + 30
      const lane = laneOf.get(it)!
      blocks.push({
        ev: it.ev,
        start: it.start,
        top: (startMin / 60) * HOUR_HEIGHT,
        height: Math.max(14, ((endMin - startMin) / 60) * HOUR_HEIGHT),
        leftPct: (lane / lanes) * 100,
        widthPct: 100 / lanes,
      })
    }
    cluster = []
    clusterEnd = 0
  }

  for (const it of items) {
    if (cluster.length && it.start.getTime() >= clusterEnd) flush()
    cluster.push(it)
    clusterEnd = Math.max(clusterEnd, it.end.getTime())
  }
  flush()
  return blocks
}

function hhmm(d: Date) {
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

// Scroll the time grid to ~7am on first render.
const scrollBody = ref<HTMLDivElement | null>(null)
onMounted(() => {
  if (scrollBody.value) scrollBody.value.scrollTop = 7 * HOUR_HEIGHT
})
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Day header + all-day strip -->
    <div class="flex border-b border-border/60 shrink-0">
      <div class="w-12 shrink-0" />
      <div class="grid grid-cols-7 flex-1">
        <div
          v-for="(d, i) in weekDays"
          :key="i"
          class="border-l border-border/40 px-1 py-1"
        >
          <div class="flex items-center justify-center gap-1 text-[11px]">
            <span class="text-muted-foreground">{{ WEEKDAYS[i] }}</span>
            <span
              :class="sameDay(d, today)
                ? 'flex h-4 w-4 items-center justify-center rounded-full bg-primary text-primary-foreground font-semibold'
                : 'font-medium'"
            >{{ d.getDate() }}</span>
          </div>
          <!-- all-day events -->
          <div class="mt-1 flex flex-col gap-0.5 min-h-0">
            <button
              v-for="p in allDayFor(d)"
              :key="p.ev.id + p.ev.calendar_id"
              type="button"
              class="truncate rounded px-1 py-0.5 text-left text-[10px] text-white"
              :style="{ backgroundColor: colorFor(p.ev.account_id) }"
              :title="p.ev.title"
              @click="emit('select', p.ev)"
            >{{ p.ev.title }}</button>
          </div>
        </div>
      </div>
    </div>

    <!-- Scrollable time grid -->
    <div ref="scrollBody" class="flex-1 overflow-y-auto min-h-0">
      <div class="flex" :style="{ height: `${24 * HOUR_HEIGHT}px` }">
        <!-- Hour labels -->
        <div class="w-12 shrink-0">
          <div
            v-for="h in HOURS"
            :key="h"
            class="relative border-b border-border/30"
            :style="{ height: `${HOUR_HEIGHT}px` }"
          >
            <span class="absolute -top-1.5 right-1 text-[10px] text-muted-foreground">
              {{ h > 0 ? String(h).padStart(2, '0') + ':00' : '' }}
            </span>
          </div>
        </div>

        <!-- Day columns -->
        <div class="grid grid-cols-7 flex-1">
          <div
            v-for="(d, i) in weekDays"
            :key="i"
            class="relative border-l border-border/40"
          >
            <!-- hour lines -->
            <div
              v-for="h in HOURS"
              :key="h"
              class="border-b border-border/30"
              :style="{ height: `${HOUR_HEIGHT}px` }"
            />
            <!-- event blocks -->
            <button
              v-for="b in timedBlocks(d)"
              :key="b.ev.id + b.ev.calendar_id"
              type="button"
              class="absolute overflow-hidden rounded px-1 py-0.5 text-left text-[10px] leading-tight text-white shadow-sm ring-1 ring-black/10 hover:brightness-110 transition"
              :style="{
                top: `${b.top}px`,
                height: `${b.height}px`,
                left: `calc(${b.leftPct}% + 1px)`,
                width: `calc(${b.widthPct}% - 2px)`,
                backgroundColor: colorFor(b.ev.account_id),
              }"
              :title="b.ev.title"
              @click="emit('select', b.ev)"
            >
              <div class="font-medium truncate">{{ b.ev.title }}</div>
              <div class="opacity-80 truncate">{{ hhmm(b.start) }}</div>
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
