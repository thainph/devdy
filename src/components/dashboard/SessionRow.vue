<script setup lang="ts">
// One running session as a row (54px, fixed), laid out like a History row:
// kind tile · its name (+ a state tag unless it is simply running, + the
// workers toggle on a parent) over project · repo#n (issue / PR) or N workers
// (conductor) · account · the context ring with its % inside · elapsed.
// The whole row is one button that opens the session (a right-click asks for
// the session menu); the workers toggle sits above it. What it is doing now
// is in the tooltip. Status accent bar, waiting glow, hover lift and fade-in
// come from `.rt-item` (runtime.css). In a narrow list (under 640px, a
// container query on SessionTree) the context ring drops out.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { sessionKind, type RtSession } from '@/lib/runtimeDashboard'
import { ringDash } from '@/lib/runtimeCharts'
import { historyRepoRef } from '@/lib/runHistory'
import SessionKindTile from './SessionKindTile.vue'
import WorkerPill from './WorkerPill.vue'
import { STATE_TAG, ctxRingClass, ctxTextClass, frameClass, useRuntimeLabels } from './runtimeStyles'

const props = withDefaults(defineProps<{
  session: RtSession
  selected: boolean
  now: number
  /** Workers under this (top-level) session, whether they show, how many need you. */
  childCount?: number
  open?: boolean
  childAttention?: number
}>(), { childCount: 0, open: false, childAttention: 0 })
const emit = defineEmits<{ select: [id: string]; toggle: [id: string]; menu: [id: string, event: MouseEvent] }>()

const { t } = useI18n()
const L = useRuntimeLabels()
const s = computed(() => props.session)
const nowText = computed(() => L.nowText(s.value, props.now))
const repoRef = computed(() => historyRepoRef({ runType: s.value.runType ?? 'session', repoPath: s.value.repoPath, refNumber: s.value.refNumber }))
const account = computed(() => (s.value.engine === 'codex' || !s.value.accountName ? t(`dashboard.engine.${s.value.engine}`) : s.value.accountName))
/** Radius of the context ring (36×36 box, 3.5px stroke). */
const RING_R = 15
</script>

<template>
  <div
    :data-session-id="s.id"
    :data-state="s.state"
    class="rt-item grid h-[54px] w-full min-w-0 grid-cols-[30px_minmax(0,1fr)_34px_52px] items-center gap-3 rounded-lg border pr-3 pl-4 @max-[640px]:grid-cols-[30px_minmax(0,1fr)_52px]"
    :class="frameClass(s, selected)"
  >
    <button
      type="button"
      class="absolute inset-0 cursor-pointer rounded-[inherit] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
      :aria-pressed="selected"
      :aria-label="`${s.title}, ${L.stateLabel(s.state)}`"
      :title="nowText ? `${s.title} — ${nowText}` : s.title"
      @click="emit('select', s.id)"
      @contextmenu.prevent="emit('menu', s.id, $event)"
    />
    <SessionKindTile :kind="sessionKind(s)" class="pointer-events-none" />
    <span class="flex min-w-0 flex-col gap-[3px]">
      <span class="flex min-w-0 items-center gap-1.5">
        <span class="pointer-events-none truncate text-[13px] leading-tight font-bold text-foreground">{{ s.title }}</span>
        <!-- Running is what this list is: only another state gets a tag. -->
        <span
          v-if="s.state !== 'running'"
          class="pointer-events-none shrink-0 rounded-full px-2 py-px text-[10px] font-bold whitespace-nowrap"
          :class="STATE_TAG[s.state]"
        >{{ L.stateLabel(s.state) }}</span>
        <WorkerPill v-if="childCount" :count="childCount" :open="open" :attention="childAttention" @toggle="emit('toggle', s.id)" />
      </span>
      <span class="pointer-events-none flex min-w-0 items-center gap-[5px] text-[11px] leading-tight text-muted-foreground">
        <span class="shrink-0">{{ s.projectName }}</span>
        <template v-if="repoRef">
          <span class="opacity-45" aria-hidden="true">·</span>
          <span class="min-w-0 truncate font-mono text-[10.5px] text-foreground/75">{{ repoRef }}</span>
        </template>
        <template v-else-if="childCount">
          <span class="opacity-45" aria-hidden="true">·</span>
          <span class="shrink-0 font-semibold text-accent-foreground">{{ t('dashboard.history.workers', { n: childCount }, childCount) }}</span>
        </template>
        <span class="opacity-45" aria-hidden="true">·</span>
        <span class="min-w-0 truncate">{{ account }}</span>
      </span>
    </span>
    <span
      class="pointer-events-none relative grid h-[34px] w-[34px] place-items-center @max-[640px]:hidden"
      :title="s.ctxPercent !== null ? t('dashboard.contextTitle', { pct: s.ctxPercent }) : t('dashboard.contextUnknown')"
    >
      <svg viewBox="0 0 36 36" class="absolute inset-0 h-full w-full" aria-hidden="true">
        <circle cx="18" cy="18" :r="RING_R" fill="none" class="stroke-muted" stroke-width="3.5" />
        <circle
          v-if="s.ctxPercent"
          cx="18"
          cy="18"
          :r="RING_R"
          fill="none"
          stroke-width="3.5"
          stroke-linecap="round"
          :stroke-dasharray="ringDash(s.ctxPercent, RING_R)"
          transform="rotate(-90 18 18)"
          :class="ctxRingClass(s.ctxPercent)"
        />
      </svg>
      <span
        class="relative font-mono font-bold tabular-nums"
        :class="[ctxTextClass(s.ctxPercent), s.ctxPercent === 100 ? 'text-[8.5px]' : 'text-[10px]']"
        aria-hidden="true"
      >{{ s.ctxPercent !== null ? `${s.ctxPercent}%` : '–' }}</span>
      <span class="sr-only">{{ s.ctxPercent !== null ? t('dashboard.contextTitle', { pct: s.ctxPercent }) : t('dashboard.contextUnknown') }}</span>
    </span>
    <span class="pointer-events-none text-right font-mono text-[11px] text-muted-foreground tabular-nums">{{ L.elapsed(s, now) }}</span>
  </div>
</template>
