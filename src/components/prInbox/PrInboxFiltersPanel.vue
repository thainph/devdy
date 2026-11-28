<script setup lang="ts">
// Filter sections of the PR inbox's settings modal (InboxOptionsButton holds
// the button, the modal and "Clear filters"): the four multi-select filters
// (project / author / account / review status), each a bordered checkbox list
// in RadarSettingsModal's style (its members list); checkbox rows follow
// GroupManagerModal (sr-only native checkbox + drawn box).
import { computed, reactive } from 'vue'
import { useI18n } from 'vue-i18n'
import { Input } from '@/components/ui'
import type { PrFilterOption, PrFilterOptions, PrFilters } from '@/lib/prInboxFilters'
import { Check } from 'lucide-vue-next'

export type PrFilterDimension = keyof PrFilters

const props = defineProps<{
  options: PrFilterOptions
  /** Stored selections (may include values no longer present — ignored). */
  modelValue: PrFilters
  /** Display text for an option of a dimension. */
  optionLabel: (dimension: PrFilterDimension, option: PrFilterOption) => string
}>()

const emit = defineEmits<{
  update: [dimension: PrFilterDimension, values: string[]]
}>()

const { t } = useI18n()

// Long lists (many authors / projects) get a per-section search box.
const SEARCH_THRESHOLD = 8
const sectionSearch = reactive<Record<PrFilterDimension, string>>({
  projects: '', authors: '', accounts: '', statuses: '',
})

const sections = computed(() => ([
  { key: 'projects', title: t('prInbox.filters.project') },
  { key: 'authors', title: t('prInbox.filters.author') },
  { key: 'accounts', title: t('prInbox.filters.account') },
  { key: 'statuses', title: t('prInbox.filters.status') },
] as const).map(s => {
  const all = props.options[s.key]
  const q = sectionSearch[s.key].trim().toLowerCase()
  const selected = (props.modelValue[s.key] as string[]).filter(v => all.some(o => o.value === v)).length
  return {
    ...s,
    selected,
    searchable: all.length > SEARCH_THRESHOLD,
    options: q ? all.filter(o => props.optionLabel(s.key, o).toLowerCase().includes(q)) : all,
    empty: all.length === 0,
  }
}))

function isChecked(dimension: PrFilterDimension, value: string): boolean {
  return (props.modelValue[dimension] as string[]).includes(value)
}

function toggle(dimension: PrFilterDimension, value: string) {
  const current = props.modelValue[dimension] as string[]
  emit('update', dimension, current.includes(value) ? current.filter(v => v !== value) : [...current, value])
}
</script>

<template>
  <section v-for="s in sections" :key="s.key" class="space-y-2">
    <h3 class="flex items-center text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
      {{ s.title }}
      <span v-if="s.selected > 0" class="ml-auto font-normal normal-case tracking-normal text-primary">
        {{ s.selected }}
      </span>
    </h3>
    <p v-if="s.empty" class="text-[11px] text-muted-foreground">{{ t('prInbox.filters.noOptions') }}</p>
    <template v-else>
      <Input
        v-if="s.searchable"
        v-model="sectionSearch[s.key]"
        size="sm"
        class="w-full"
        :placeholder="t('prInbox.filters.searchSection', { section: s.title })"
        :aria-label="t('prInbox.filters.searchSection', { section: s.title })"
      />
      <div class="max-h-48 overflow-auto rounded-md border border-border/70" role="group" :aria-label="s.title">
        <label
          v-for="o in s.options"
          :key="o.value"
          class="flex cursor-pointer items-center gap-2.5 border-b border-border/40 px-3 py-1.5 transition-colors last:border-b-0 hover:bg-accent/50 has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-inset has-[input:focus-visible]:ring-ring"
        >
          <span
            class="flex h-4 w-4 shrink-0 items-center justify-center rounded border transition-colors"
            :class="isChecked(s.key, o.value) ? 'bg-primary border-primary text-white' : 'border-border'"
          >
            <Check v-if="isChecked(s.key, o.value)" class="h-3 w-3" :stroke-width="3" />
          </span>
          <input
            type="checkbox"
            class="sr-only"
            :checked="isChecked(s.key, o.value)"
            @change="toggle(s.key, o.value)"
          />
          <img v-if="o.avatarUrl" :src="o.avatarUrl" alt="" class="h-5 w-5 shrink-0 rounded-full bg-muted" />
          <span class="min-w-0 flex-1 truncate text-[13px]">{{ optionLabel(s.key, o) }}</span>
          <span class="shrink-0 text-[11px] tabular-nums text-muted-foreground">{{ o.count }}</span>
        </label>
        <p v-if="s.options.length === 0" class="px-3 py-2 text-[11px] text-muted-foreground">
          {{ t('prInbox.filters.noMatchSection') }}
        </p>
      </div>
    </template>
  </section>
</template>
