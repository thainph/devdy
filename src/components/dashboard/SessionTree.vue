<script setup lang="ts">
// The Runtime Dashboard's list of running sessions, as a tree of rows: each
// session, with the workers it spawned right under it. Top-level sessions are
// flush-left and aligned; only workers are indented, with a rounded connector.
// A parent shows / hides its workers with the pill after its title. The list
// is a size container, so the rows can drop columns when it is narrow.
import type { TreeRow } from '@/lib/runtimeDashboard'
import SessionRow from './SessionRow.vue'

defineProps<{
  rows: TreeRow[]
  selectedId: string | null
  now: number
}>()
const emit = defineEmits<{ select: [id: string]; toggle: [id: string]; menu: [id: string, event: MouseEvent] }>()
</script>

<template>
  <div class="@container flex flex-col gap-1.5" role="list">
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
        @menu="(id, e) => emit('menu', id, e)"
      />
    </div>
  </div>
</template>
