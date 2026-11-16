<script setup lang="ts">
// Project scope of the Delivery Radar: one compact trigger ("All projects",
// one name, or "A, B +3") opening a searchable checklist, instead of a chip per
// project that wraps into several lines once there are many. An empty
// selection means every project (same as ticking them all).
import { computed, nextTick, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, FolderKanban, RefreshCw, Search } from 'lucide-vue-next'
import { Button, Checkbox, DropdownMenu } from '@/components/ui'

const props = defineProps<{
  projects: { id: string, name: string }[]
  /** Ticked ids; empty = all. */
  selected: Set<string>
  /** Ids currently loading (spinner next to the name). */
  loading?: Set<string>
}>()
const emit = defineEmits<{ 'update:selected': [value: Set<string>] }>()

const { t } = useI18n()

const query = ref('')
const searchEl = ref<HTMLInputElement | null>(null)
const SEARCH_MIN = 7

const isAll = computed(() => props.selected.size === 0)
function isOn(id: string): boolean {
  return isAll.value || props.selected.has(id)
}

const label = computed(() => {
  if (isAll.value) return t('radar.projectPicker.all', { n: props.projects.length })
  const names = props.projects.filter((p) => props.selected.has(p.id)).map((p) => p.name)
  if (names.length <= 2) return names.join(', ')
  return t('radar.projectPicker.more', { names: names.slice(0, 2).join(', '), n: names.length - 2 })
})

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  return q ? props.projects.filter((p) => p.name.toLowerCase().includes(q)) : props.projects
})

function set(next: Set<string>) {
  // Every project ticked is the same as "All".
  emit('update:selected', next.size === props.projects.length ? new Set() : next)
}
function toggle(id: string) {
  const next = new Set(isAll.value ? props.projects.map((p) => p.id) : props.selected)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  // Unticking the last one falls back to "All" rather than an empty radar.
  set(next)
}
function only(id: string) {
  emit('update:selected', new Set([id]))
}

async function onOpen(open: boolean) {
  if (!open) {
    query.value = ''
    return
  }
  await nextTick()
  searchEl.value?.focus()
}
</script>

<template>
  <DropdownMenu align="left" @update:open="onOpen">
    <template #trigger="{ open }">
      <Button variant="outline" class="max-w-72" :class="{ 'border-primary/40': open }">
        <FolderKanban class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span class="truncate">{{ label }}</span>
        <RefreshCw v-if="loading?.size" class="h-3 w-3 shrink-0 animate-spin text-muted-foreground" :stroke-width="2" />
        <ChevronDown class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="2" />
      </Button>
    </template>

    <!-- The menu closes on any click inside; this picker stays open while ticking. -->
    <div class="w-72" @click.stop>
      <div v-if="projects.length >= SEARCH_MIN" class="relative mb-1">
        <Search class="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
        <input
          ref="searchEl"
          v-model="query"
          class="w-full h-8 rounded border border-border bg-background pl-7 pr-2 text-xs outline-none focus:border-primary/50"
          :placeholder="t('radar.projectPicker.search')"
        >
      </div>

      <div class="flex items-center justify-between px-2 py-1 text-[11px] text-muted-foreground">
        <span>{{ t('radar.projectPicker.selected', { n: isAll ? projects.length : selected.size, total: projects.length }) }}</span>
        <button
          class="hover:text-foreground disabled:opacity-40 disabled:pointer-events-none"
          :disabled="isAll"
          @click="emit('update:selected', new Set())"
        >
          {{ t('radar.projectPicker.selectAll') }}
        </button>
      </div>

      <div class="max-h-80 overflow-auto">
        <div
          v-for="p in filtered" :key="p.id"
          class="group flex items-center gap-2 rounded px-2 py-1.5 hover:bg-accent"
        >
          <Checkbox :model-value="isOn(p.id)" class="min-w-0 flex-1 items-center" @update:model-value="toggle(p.id)">
            <span class="flex items-center gap-1.5 min-w-0 text-[13px]">
              <span class="truncate" :title="p.name">{{ p.name }}</span>
              <RefreshCw v-if="loading?.has(p.id)" class="h-2.5 w-2.5 shrink-0 animate-spin text-muted-foreground" :stroke-width="2" />
            </span>
          </Checkbox>
          <button
            class="shrink-0 rounded px-1.5 text-[11px] text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-primary focus:opacity-100"
            :title="t('radar.projectPicker.onlyHint', { project: p.name })"
            @click="only(p.id)"
          >
            {{ t('radar.projectPicker.only') }}
          </button>
        </div>
        <p v-if="filtered.length === 0" class="px-2 py-3 text-center text-[11px] text-muted-foreground">
          {{ t('radar.projectPicker.noMatch') }}
        </p>
      </div>
    </div>
  </DropdownMenu>
</template>
