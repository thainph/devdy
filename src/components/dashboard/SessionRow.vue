<script setup lang="ts">
// One session as a compact row (~44px, fixed): engine · name + project (+ the
// workers toggle on a parent) · state · now · context · activity · elapsed.
// The whole row is one button that opens the session; the workers toggle sits
// above it. Status accent bar, waiting glow, hover lift and fade-in come from
// `.rt-item` (runtime.css).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { RtSession } from '@/lib/runtimeDashboard'
import EngineMark from './EngineMark.vue'
import NowIcon from './NowIcon.vue'
import SparkLine from './SparkLine.vue'
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
    class="rt-item grid h-11 w-full min-w-0 grid-cols-[22px_minmax(140px,1.4fr)_104px_minmax(0,2.2fr)_74px_66px_52px] items-center gap-3 rounded-lg border pr-3 pl-4"
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
    <EngineMark :engine="s.engine" size="sm" class="pointer-events-none" />
    <span class="flex min-w-0 items-center gap-1.5">
      <span class="pointer-events-none flex min-w-0 items-baseline gap-1.5">
        <span class="truncate text-[13px] font-bold text-foreground">{{ s.title }}</span>
        <span class="truncate text-[11px] text-muted-foreground">{{ s.projectName }}</span>
      </span>
      <WorkerPill v-if="childCount" :count="childCount" :open="open" :attention="childAttention" @toggle="emit('toggle', s.id)" />
    </span>
    <span class="pointer-events-none min-w-0">
      <span class="inline-block max-w-full truncate rounded-full px-2 py-0.5 align-middle text-[10px] font-bold" :class="STATE_TAG[s.state]">{{ L.stateLabel(s.state) }}</span>
    </span>
    <span class="pointer-events-none flex min-w-0 items-center gap-1.5">
      <span :class="alert ? 'text-warning' : 'text-primary'"><NowIcon :now="s.now" /></span>
      <span class="truncate font-mono text-[11px] text-foreground/90" :class="s.now.kind === 'tool' && 'rt-shimmer'">{{ nowText }}</span>
    </span>
    <span class="pointer-events-none flex items-center gap-1.5">
      <span class="relative h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
        <span class="absolute inset-y-0 left-0 rounded-full" :class="ctxBarClass(s.ctxPercent)" :style="{ width: `${s.ctxPercent ?? 0}%` }" />
      </span>
      <span class="w-6 text-right font-mono text-[10px] tabular-nums" :class="ctxTextClass(s.ctxPercent)">{{ s.ctxPercent ?? '–' }}</span>
      <span class="sr-only">{{ s.ctxPercent !== null ? t('dashboard.contextTitle', { pct: s.ctxPercent }) : t('dashboard.contextUnknown') }}</span>
    </span>
    <SparkLine :values="s.spark" :width="66" :height="22" class="pointer-events-none" />
    <span class="pointer-events-none text-right font-mono text-[11px] text-muted-foreground tabular-nums">{{ L.elapsed(s, now) }}</span>
  </div>
</template>
