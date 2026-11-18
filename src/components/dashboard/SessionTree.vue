<script setup lang="ts">
// The Runtime Dashboard's one list of sessions, as a tree: each session, with
// the workers it spawned right under it (expand / collapse per parent). Rows
// mode draws tree connectors; cards mode puts a parent and its workers in one
// full-width block.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronRight } from 'lucide-vue-next'
import type { TreeRow } from '@/lib/runtimeDashboard'
import SessionCard from './SessionCard.vue'
import SessionRow from './SessionRow.vue'

const props = defineProps<{
  rows: TreeRow[]
  mode: 'cards' | 'rows'
  selectedId: string | null
  now: number
}>()
const emit = defineEmits<{ select: [id: string]; toggle: [id: string] }>()

const { t } = useI18n()

/** The toggle column is only reserved when some session has workers. */
const anyParent = computed(() => props.rows.some((r) => r.depth === 0 && r.childCount > 0))

/** Cards mode: a top-level row with the worker rows that follow it. */
const blocks = computed(() => {
  const out: { parent: TreeRow; kids: TreeRow[] }[] = []
  for (const r of props.rows) {
    if (r.depth === 0) out.push({ parent: r, kids: [] })
    else out[out.length - 1]?.kids.push(r)
  }
  return out
})

function toggleLabel(r: TreeRow): string {
  return r.open ? t('dashboard.tree.hide', { n: r.childCount }, r.childCount) : t('dashboard.tree.show', { n: r.childCount }, r.childCount)
}
</script>

<template>
  <!-- Rows: a tree with connectors -->
  <div v-if="mode === 'rows'" class="flex flex-col gap-1" role="list">
    <div v-for="r in rows" :key="r.session.id" class="flex min-w-0 items-stretch" role="listitem">
      <template v-if="r.depth === 0">
        <button
          v-if="r.childCount"
          type="button"
          class="mr-1 flex w-10 shrink-0 cursor-pointer items-center justify-center gap-0.5 rounded-md text-[11px] font-semibold tabular-nums transition-colors hover:bg-accent/50 focus-visible:outline-2 focus-visible:outline-primary"
          :class="!r.open && r.childAttention ? 'text-warning' : 'text-muted-foreground'"
          :aria-expanded="r.open"
          :aria-label="toggleLabel(r)"
          :title="toggleLabel(r)"
          @click="emit('toggle', r.session.id)"
        >
          <ChevronRight class="h-3.5 w-3.5 transition-transform motion-reduce:transition-none" :class="r.open && 'rotate-90'" :stroke-width="2" aria-hidden="true" />
          {{ r.childCount }}
        </button>
        <span v-else-if="anyParent" class="mr-1 w-10 shrink-0" aria-hidden="true" />
      </template>
      <template v-else>
        <!-- └ / ├ connector from the parent's toggle column -->
        <span class="relative ml-5 w-6 shrink-0" aria-hidden="true">
          <span class="absolute -top-1 left-0 w-px bg-border" :class="r.last ? 'h-[calc(50%+4px)]' : 'bottom-0'" />
          <span class="absolute top-1/2 left-0 h-px w-4 bg-border" />
        </span>
      </template>
      <SessionRow class="min-w-0 flex-1" :session="r.session" :selected="selectedId === r.session.id" :now="now" @select="emit('select', $event)" />
    </div>
  </div>

  <!-- Cards: a parent with workers is one full-width block -->
  <div v-else class="grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-2">
    <template v-for="b in blocks" :key="b.parent.session.id">
      <SessionCard
        v-if="!b.parent.childCount"
        :session="b.parent.session"
        :selected="selectedId === b.parent.session.id"
        :now="now"
        @select="emit('select', $event)"
      />
      <div v-else class="col-span-full flex flex-col gap-2 rounded-lg border border-border/60 bg-muted/30 p-2">
        <div class="grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-2">
          <SessionCard :session="b.parent.session" :selected="selectedId === b.parent.session.id" :now="now" @select="emit('select', $event)" />
        </div>
        <button
          type="button"
          class="flex w-fit cursor-pointer items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] font-semibold transition-colors hover:bg-accent/50 focus-visible:outline-2 focus-visible:outline-primary"
          :class="!b.parent.open && b.parent.childAttention ? 'text-warning' : 'text-muted-foreground'"
          :aria-expanded="b.parent.open"
          @click="emit('toggle', b.parent.session.id)"
        >
          <ChevronRight class="h-3.5 w-3.5 transition-transform motion-reduce:transition-none" :class="b.parent.open && 'rotate-90'" :stroke-width="2" aria-hidden="true" />
          {{ toggleLabel(b.parent) }}
          <span v-if="!b.parent.open && b.parent.childAttention">· {{ t('dashboard.tree.needsYou', { n: b.parent.childAttention }) }}</span>
        </button>
        <div v-if="b.kids.length" class="ml-2 grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-2 border-l-2 border-border pl-3">
          <SessionCard
            v-for="k in b.kids"
            :key="k.session.id"
            :session="k.session"
            :selected="selectedId === k.session.id"
            :now="now"
            @select="emit('select', $event)"
          />
        </div>
      </div>
    </template>
  </div>
</template>
