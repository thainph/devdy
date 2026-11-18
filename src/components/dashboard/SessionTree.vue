<script setup lang="ts">
// The Runtime Dashboard's one list of sessions, as a tree: each session, with
// the workers it spawned right under it. Top-level sessions are flush-left and
// aligned; only workers are indented, with a rounded connector. A parent shows
// / hides its workers with the pill after its title. Cards mode puts a parent
// and its workers in one full-width block.
import { computed } from 'vue'
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

/** Cards mode: a top-level row with the worker rows that follow it. */
const blocks = computed(() => {
  const out: { parent: TreeRow; kids: TreeRow[] }[] = []
  for (const r of props.rows) {
    if (r.depth === 0) out.push({ parent: r, kids: [] })
    else out[out.length - 1]?.kids.push(r)
  }
  return out
})
</script>

<template>
  <!-- Rows: a tree, workers indented under a rounded connector -->
  <div v-if="mode === 'rows'" class="flex flex-col gap-1.5" role="list">
    <div v-for="r in rows" :key="r.session.id" class="flex min-w-0 items-stretch" role="listitem">
      <span v-if="r.depth === 1" class="rt-conn" :class="r.last && 'is-last'" aria-hidden="true" />
      <SessionRow
        class="min-w-0 flex-1"
        :session="r.session"
        :selected="selectedId === r.session.id"
        :now="now"
        :child-count="r.childCount"
        :open="r.open"
        :child-attention="r.childAttention"
        @select="emit('select', $event)"
        @toggle="emit('toggle', $event)"
      />
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
          <SessionCard
            :session="b.parent.session"
            :selected="selectedId === b.parent.session.id"
            :now="now"
            :child-count="b.parent.childCount"
            :open="b.parent.open"
            :child-attention="b.parent.childAttention"
            @select="emit('select', $event)"
            @toggle="emit('toggle', $event)"
          />
        </div>
        <div v-if="b.kids.length" class="ml-2 grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-2 rounded-bl-lg border-l-2 border-border pl-3">
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
