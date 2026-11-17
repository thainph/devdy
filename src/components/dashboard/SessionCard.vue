<script setup lang="ts">
// One session as a card. Its height is FIXED: hovering, answering or a longer
// "now" line never moves the grid. Waiting = warning tint, quiet = dashed
// border, selected = primary ring.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { RtSession } from '@/lib/runtimeDashboard'
import EngineMark from './EngineMark.vue'
import NowIcon from './NowIcon.vue'
import SparkLine from './SparkLine.vue'
import StateDot from './StateDot.vue'
import { STATE_TAG, ctxBarClass, ctxTextClass, frameClass, useRuntimeLabels } from './runtimeStyles'

const props = defineProps<{ session: RtSession; selected: boolean; now: number }>()
const emit = defineEmits<{ select: [id: string] }>()

const { t } = useI18n()
const L = useRuntimeLabels()
const s = computed(() => props.session)
const alert = computed(() => s.value.state === 'waiting' || s.value.state === 'quiet')
const nowText = computed(() => L.nowText(s.value, props.now))
</script>

<template>
  <button
    type="button"
    :data-session-id="s.id"
    class="flex h-[140px] w-full min-w-0 cursor-pointer flex-col gap-1.5 rounded-lg border px-3 py-2.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
    :class="frameClass(s, selected)"
    :aria-pressed="selected"
    :aria-label="`${s.title}, ${L.stateLabel(s.state)}`"
    @click="emit('select', s.id)"
  >
    <span class="flex w-full min-w-0 items-center gap-2">
      <EngineMark :engine="s.engine" />
      <span class="min-w-0 flex-1">
        <span class="block truncate text-[13px] font-semibold text-foreground" :title="s.title">{{ s.title }}</span>
        <span class="block truncate text-[11px] text-muted-foreground">{{ L.meta(s) }}</span>
      </span>
      <StateDot :state="s.state" />
    </span>
    <span class="flex w-full items-center gap-1.5">
      <span class="rounded-full px-2 py-0.5 text-[10px] font-semibold whitespace-nowrap" :class="STATE_TAG[s.state]">{{ L.stateLabel(s.state) }}</span>
      <span class="font-mono text-[11px] text-muted-foreground tabular-nums">{{ L.elapsed(s, now) }}</span>
      <SparkLine :values="s.spark" class="ml-auto" />
    </span>
    <span
      class="flex h-7 w-full min-w-0 items-center gap-1.5 rounded-md px-2"
      :class="s.state === 'waiting' ? 'bg-card' : 'bg-muted/60'"
      :title="nowText"
    >
      <span :class="alert ? 'text-warning' : 'text-primary'"><NowIcon :now="s.now" /></span>
      <span class="min-w-0 flex-1 truncate font-mono text-[11px] text-foreground/90">{{ nowText }}</span>
      <span class="shrink-0 font-mono text-[10px] text-muted-foreground tabular-nums">{{ L.nowFor(s, now) }}</span>
    </span>
    <span
      class="mt-auto flex w-full items-center gap-2"
      :title="s.ctxPercent !== null ? t('dashboard.contextTitle', { pct: s.ctxPercent }) : t('dashboard.contextUnknown')"
    >
      <span class="text-[10px] text-muted-foreground">{{ t('dashboard.context') }}</span>
      <span class="relative h-1 flex-1 overflow-hidden rounded-full bg-muted">
        <span class="absolute inset-y-0 left-0 rounded-full" :class="ctxBarClass(s.ctxPercent)" :style="{ width: `${s.ctxPercent ?? 0}%` }" />
      </span>
      <span class="w-8 text-right font-mono text-[10px] tabular-nums" :class="ctxTextClass(s.ctxPercent)">{{ s.ctxPercent !== null ? `${s.ctxPercent}%` : '–' }}</span>
    </span>
  </button>
</template>
