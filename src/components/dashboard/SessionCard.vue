<script setup lang="ts">
// One session as a card. Its height is FIXED: hovering, answering or a longer
// "now" line never moves the grid. The whole card is one button that opens the
// session; a parent's workers toggle sits above it, after the title. Same
// visual language as the rows (`.rt-item`, runtime.css): status accent bar,
// waiting glow, hover lift, fade-in; quiet = dashed border, selected = thin
// primary border + tint.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { RtSession } from '@/lib/runtimeDashboard'
import EngineMark from './EngineMark.vue'
import NowIcon from './NowIcon.vue'
import SparkLine from './SparkLine.vue'
import StateDot from './StateDot.vue'
import WorkerPill from './WorkerPill.vue'
import { STATE_TAG, ctxBarClass, ctxTextClass, frameClass, useRuntimeLabels } from './runtimeStyles'

const props = withDefaults(defineProps<{
  session: RtSession
  selected: boolean
  now: number
  /** Workers under this (top-level) session, whether they show, how many need you. */
  childCount?: number
  open?: boolean
  childAttention?: number
}>(), { childCount: 0, open: false, childAttention: 0 })
const emit = defineEmits<{ select: [id: string]; toggle: [id: string] }>()

const { t } = useI18n()
const L = useRuntimeLabels()
const s = computed(() => props.session)
const alert = computed(() => s.value.state === 'waiting' || s.value.state === 'quiet')
const nowText = computed(() => L.nowText(s.value, props.now))
</script>

<template>
  <div
    :data-session-id="s.id"
    :data-state="s.state"
    class="rt-item flex h-[140px] w-full min-w-0 flex-col gap-1.5 rounded-lg border py-2.5 pr-3 pl-4"
    :class="frameClass(s, selected)"
  >
    <button
      type="button"
      class="absolute inset-0 cursor-pointer rounded-[inherit] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
      :aria-pressed="selected"
      :aria-label="`${s.title}, ${L.stateLabel(s.state)}`"
      :title="nowText ? `${s.title} — ${nowText}` : s.title"
      @click="emit('select', s.id)"
    />
    <span class="pointer-events-none flex w-full min-w-0 items-center gap-2">
      <EngineMark :engine="s.engine" />
      <span class="min-w-0 flex-1">
        <span class="flex min-w-0 items-center gap-1.5">
          <span class="truncate text-[13px] font-bold text-foreground">{{ s.title }}</span>
          <WorkerPill v-if="childCount" class="pointer-events-auto" :count="childCount" :open="open" :attention="childAttention" @toggle="emit('toggle', s.id)" />
        </span>
        <span class="block truncate text-[11px] text-muted-foreground">{{ L.meta(s) }}</span>
      </span>
      <StateDot :state="s.state" />
    </span>
    <span class="pointer-events-none flex w-full items-center gap-1.5">
      <span class="rounded-full px-2 py-0.5 text-[10px] font-bold whitespace-nowrap" :class="STATE_TAG[s.state]">{{ L.stateLabel(s.state) }}</span>
      <span class="font-mono text-[11px] text-muted-foreground tabular-nums">{{ L.elapsed(s, now) }}</span>
      <SparkLine :values="s.spark" class="ml-auto" />
    </span>
    <span
      class="pointer-events-none flex h-7 w-full min-w-0 items-center gap-1.5 rounded-md px-2"
      :class="s.state === 'waiting' ? 'bg-card' : 'bg-muted/60'"
    >
      <span :class="alert ? 'text-warning' : 'text-primary'"><NowIcon :now="s.now" /></span>
      <span class="min-w-0 flex-1 truncate font-mono text-[11px] text-foreground/90" :class="s.now.kind === 'tool' && 'rt-shimmer'">{{ nowText }}</span>
      <span class="shrink-0 font-mono text-[10px] text-muted-foreground tabular-nums">{{ L.nowFor(s, now) }}</span>
    </span>
    <span class="pointer-events-none mt-auto flex w-full items-center gap-2">
      <span class="text-[10px] text-muted-foreground">{{ t('dashboard.context') }}</span>
      <span class="relative h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
        <span class="absolute inset-y-0 left-0 rounded-full" :class="ctxBarClass(s.ctxPercent)" :style="{ width: `${s.ctxPercent ?? 0}%` }" />
      </span>
      <span class="w-8 text-right font-mono text-[10px] tabular-nums" :class="ctxTextClass(s.ctxPercent)">{{ s.ctxPercent !== null ? `${s.ctxPercent}%` : '–' }}</span>
    </span>
  </div>
</template>
