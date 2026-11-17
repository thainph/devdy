<script setup lang="ts">
// One group of the Runtime Dashboard: a conductor with its workers, the
// standalone sessions, or the sessions outside Devdy. One-line header (click to
// collapse / expand) with a dot per member and attention tags; the open body
// lists what needs you and folds the sessions running fine behind "+N".
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { CheckCircle2, ChevronRight, ExternalLink, Folder, Layers } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import type { GroupView } from '@/lib/runtimeDashboard'
import SessionCard from './SessionCard.vue'
import SessionRow from './SessionRow.vue'
import { STATE_FILL, useRuntimeLabels } from './runtimeStyles'

const props = defineProps<{
  group: GroupView
  mode: 'cards' | 'rows'
  selectedId: string | null
  now: number
}>()
const emit = defineEmits<{
  toggle: [key: string]
  unfold: [key: string]
  refold: [key: string]
  select: [id: string]
}>()

const { t } = useI18n()
const L = useRuntimeLabels()
const g = computed(() => props.group)

const title = computed(() => {
  if (g.value.kind === 'solo') return t('dashboard.group.solo')
  if (g.value.kind === 'external') return t('dashboard.group.external')
  return g.value.conductor?.title ?? ''
})
const sub = computed(() => {
  if (g.value.kind === 'solo') return t('dashboard.group.soloSub')
  if (g.value.kind === 'external') return t('dashboard.group.externalSub')
  const c = g.value.conductor
  return c ? [c.projectName, L.elapsed(c, props.now)].filter(Boolean).join(' · ') : ''
})
const icon = computed(() => (g.value.kind === 'conductor' ? Layers : g.value.kind === 'solo' ? Folder : ExternalLink))
const countTag = computed(() => {
  const n = g.value.members.length
  return g.value.kind === 'conductor' ? t('dashboard.group.workers', { n }, n) : t('dashboard.group.sessions', { n }, n)
})
const FOLD_DOTS = 12
</script>

<template>
  <section
    class="rounded-lg border"
    :class="g.kind === 'conductor' ? 'border-border/70 bg-muted/30' : 'border-dashed border-border/70'"
    :aria-label="title"
  >
    <div class="flex min-h-[46px] flex-wrap items-center gap-2 px-2.5 py-1.5">
      <button
        type="button"
        class="flex min-w-0 cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-left hover:bg-accent/50 focus-visible:outline-2 focus-visible:outline-primary"
        :aria-expanded="g.open"
        @click="emit('toggle', g.key)"
      >
        <ChevronRight
          class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform motion-reduce:transition-none"
          :class="g.open && 'rotate-90'"
          :stroke-width="2"
          aria-hidden="true"
        />
        <component
          :is="icon"
          class="h-4 w-4 shrink-0"
          :class="g.kind === 'conductor' ? 'text-primary' : 'text-muted-foreground'"
          :stroke-width="1.75"
          aria-hidden="true"
        />
        <span class="truncate text-sm font-semibold text-foreground">{{ title }}</span>
        <span class="truncate text-[11px] text-muted-foreground">{{ sub }}</span>
      </button>
      <span class="flex max-w-[220px] flex-wrap gap-[3px]" aria-hidden="true">
        <i v-for="(st, i) in g.dots" :key="i" class="block h-2 w-2 rounded-sm" :class="STATE_FILL[st]" />
      </span>
      <span class="ml-auto flex flex-wrap items-center gap-1.5">
        <span v-if="g.counts.waiting" class="rounded-full bg-warning/15 px-2 py-0.5 text-[10px] font-semibold text-warning">{{ t('dashboard.group.waiting', { n: g.counts.waiting }) }}</span>
        <span v-if="g.counts.quiet" class="rounded-full bg-warning/15 px-2 py-0.5 text-[10px] font-semibold text-warning">{{ t('dashboard.group.quiet', { n: g.counts.quiet }) }}</span>
        <span v-if="g.counts.ctx" class="rounded-full bg-warning/15 px-2 py-0.5 text-[10px] font-semibold text-warning">{{ t('dashboard.group.ctx', { n: g.counts.ctx }) }}</span>
        <span class="rounded-full bg-muted px-2 py-0.5 text-[10px] font-semibold text-muted-foreground">{{ countTag }}</span>
        <Button
          v-if="g.conductor"
          variant="outline"
          size="xs"
          :aria-pressed="selectedId === g.conductor.id"
          @click="emit('select', g.conductor.id)"
        >
          {{ t('dashboard.group.viewConductor') }}
        </Button>
      </span>
    </div>

    <div v-if="g.open" class="flex flex-col gap-2 px-2.5 pb-2.5">
      <div v-if="g.shown.length && mode === 'cards'" class="grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-2">
        <SessionCard
          v-for="s in g.shown"
          :key="s.id"
          :session="s"
          :selected="selectedId === s.id"
          :now="now"
          @select="emit('select', $event)"
        />
      </div>
      <div v-else-if="g.shown.length" class="flex flex-col gap-1">
        <SessionRow
          v-for="s in g.shown"
          :key="s.id"
          :session="s"
          :selected="selectedId === s.id"
          :now="now"
          @select="emit('select', $event)"
        />
      </div>
      <button
        v-if="g.hasFold"
        type="button"
        class="flex min-h-9 w-full cursor-pointer items-center gap-2.5 rounded-md border border-dashed border-border bg-card px-3 text-xs font-semibold text-muted-foreground transition-colors hover:border-primary/40 hover:text-primary"
        @click="emit('unfold', g.key)"
      >
        <CheckCircle2 class="h-3.5 w-3.5 shrink-0 text-success" :stroke-width="1.75" aria-hidden="true" />
        <span>{{ t('dashboard.group.fold', { n: g.foldN }) }}</span>
        <span class="flex flex-wrap gap-[3px]" aria-hidden="true">
          <i v-for="(st, i) in g.foldStates.slice(0, FOLD_DOTS)" :key="i" class="block h-2 w-2 rounded-sm" :class="STATE_FILL[st]" />
        </span>
        <span class="ml-auto text-primary">{{ t('dashboard.group.show') }}</span>
      </button>
      <button
        v-if="g.canRefold"
        type="button"
        class="flex min-h-9 w-full cursor-pointer items-center rounded-md border border-dashed border-border bg-card px-3 text-xs font-semibold text-muted-foreground transition-colors hover:border-primary/40 hover:text-primary"
        @click="emit('refold', g.key)"
      >
        {{ t('dashboard.group.refold', { n: g.foldN }, g.foldN) }}
      </button>
    </div>
  </section>
</template>
