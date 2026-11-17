<script setup lang="ts">
// One session as a compact row (~38px, fixed): engine · name + project · state ·
// now · context · activity · elapsed.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { RtSession } from '@/lib/runtimeDashboard'
import EngineMark from './EngineMark.vue'
import NowIcon from './NowIcon.vue'
import SparkLine from './SparkLine.vue'
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
    class="grid h-[38px] w-full min-w-0 cursor-pointer grid-cols-[20px_minmax(110px,1.3fr)_92px_minmax(0,2fr)_64px_60px_52px] items-center gap-2.5 rounded-md border px-2.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
    :class="frameClass(s, selected)"
    :aria-pressed="selected"
    :aria-label="`${s.title}, ${L.stateLabel(s.state)}`"
    @click="emit('select', s.id)"
  >
    <EngineMark :engine="s.engine" size="sm" />
    <span class="flex min-w-0 items-baseline gap-1.5">
      <span class="truncate text-xs font-semibold text-foreground" :title="s.title">{{ s.title }}</span>
      <span class="truncate text-[11px] text-muted-foreground">{{ s.projectName }}</span>
    </span>
    <span class="min-w-0">
      <span class="inline-block max-w-full truncate rounded-full px-2 py-0.5 align-middle text-[10px] font-semibold" :class="STATE_TAG[s.state]">{{ L.stateLabel(s.state) }}</span>
    </span>
    <span class="flex min-w-0 items-center gap-1.5" :title="nowText">
      <span :class="alert ? 'text-warning' : 'text-primary'"><NowIcon :now="s.now" /></span>
      <span class="truncate font-mono text-[11px] text-foreground/90">{{ nowText }}</span>
    </span>
    <span
      class="flex items-center gap-1"
      :title="s.ctxPercent !== null ? t('dashboard.contextTitle', { pct: s.ctxPercent }) : t('dashboard.contextUnknown')"
    >
      <span class="relative h-1 flex-1 overflow-hidden rounded-full bg-muted">
        <span class="absolute inset-y-0 left-0 rounded-full" :class="ctxBarClass(s.ctxPercent)" :style="{ width: `${s.ctxPercent ?? 0}%` }" />
      </span>
      <span class="w-6 text-right font-mono text-[10px] tabular-nums" :class="ctxTextClass(s.ctxPercent)">{{ s.ctxPercent ?? '–' }}</span>
    </span>
    <SparkLine :values="s.spark" :width="60" :height="18" />
    <span class="text-right font-mono text-[11px] text-muted-foreground tabular-nums">{{ L.elapsed(s, now) }}</span>
  </button>
</template>
