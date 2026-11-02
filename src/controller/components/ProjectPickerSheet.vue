<script setup lang="ts">
/**
 * Bottom sheet that picks the project every tab follows (see
 * `@/lib/controllerProjectScope`): a search field, "All projects" when the
 * screen allows it, then recently used projects and the rest — big rows, the
 * current one ticked. Opened from the tab header's title or Setting's
 * "Project" row.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Layers } from 'lucide-vue-next'
import { pickerRows } from '@/lib/controllerProjectScope'
import type { ProjectInfo } from '../protocol'
import SearchField from './SearchField.vue'

const props = defineProps<{
  projects: ProjectInfo[]
  /** The ticked project; null = all projects. */
  selected: string | null
  /** Offer "All projects" (not on screens that need one project). */
  allowAll: boolean
  recent: string[]
}>()

const emit = defineEmits<{
  pick: [projectId: string | null]
  close: []
}>()

const { t } = useI18n()

const query = ref('')
const rows = computed(() => pickerRows(props.projects, props.recent, query.value))
const empty = computed(() => !rows.value.recent.length && !rows.value.others.length)
const sections = computed(() =>
  [
    { key: 'recent', label: t('controller.projectPicker.recent'), items: rows.value.recent },
    {
      key: 'others',
      label: rows.value.recent.length ? t('controller.projectPicker.others') : '',
      items: rows.value.others,
    },
  ].filter((s) => s.items.length),
)

const heading = 'mt-3 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
const row =
  'flex w-full cursor-pointer items-center gap-2.5 px-3 py-3 border-b border-border/60 last:border-b-0 text-left text-sm font-medium'
</script>

<template>
  <div class="absolute inset-0 z-20 flex flex-col justify-end">
    <div class="ui-backdrop absolute inset-0 bg-black/50" @click="emit('close')" />
    <div
      class="ctl-sheet relative flex max-h-[85%] flex-col rounded-t-2xl border-t border-border bg-card pt-2.5 pb-[max(1rem,env(safe-area-inset-bottom))] shadow-2xl"
    >
      <div class="mx-auto mb-2 h-1.5 w-10 shrink-0 rounded-full bg-foreground/20" />
      <p class="px-4 text-sm font-semibold">{{ t('controller.projectPicker.title') }}</p>
      <SearchField
        v-model="query"
        class="!border-b-0"
        :placeholder="t('controller.projectPicker.search')"
        :clear-label="t('controller.projectPicker.clear')"
      />

      <div class="min-h-0 overflow-auto px-3 pb-1">
        <div v-if="allowAll && !query.trim()" class="ui-card overflow-hidden rounded-xl border border-border bg-card">
          <button type="button" :class="row" @click="emit('pick', null)">
            <Layers class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            <span class="min-w-0 flex-1 truncate">{{ t('controller.data.allProjects') }}</span>
            <Check v-if="selected === null" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2.5" />
          </button>
        </div>

        <template v-for="s in sections" :key="s.key">
          <h2 v-if="s.label" :class="heading">{{ s.label }}</h2>
          <div class="ui-card overflow-hidden rounded-xl border border-border bg-card" :class="s.label ? '' : 'mt-3'">
            <button v-for="p in s.items" :key="p.id" type="button" :class="row" @click="emit('pick', p.id)">
              <span class="min-w-0 flex-1">
                <span class="block truncate">{{ p.name }}</span>
                <span v-if="p.path" class="block truncate text-[11px] font-normal text-muted-foreground">{{ p.path }}</span>
              </span>
              <Check v-if="selected === p.id" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2.5" />
            </button>
          </div>
        </template>

        <p v-if="empty" class="py-8 text-center text-sm text-foreground/50">{{ t('controller.projectPicker.empty') }}</p>
      </div>
    </div>
  </div>
</template>
