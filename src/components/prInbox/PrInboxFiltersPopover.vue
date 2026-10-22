<script setup lang="ts">
// "Filters" toolbar button + popover for the PR inbox: the four multi-select
// filters (project / author / account / review status) as checkbox lists in a
// single panel. Panel + trigger follow CalendarView's display-options popover
// (onClickOutside, `bg-popover` panel, uppercase section labels); checkbox rows
// follow GroupManagerModal (sr-only native checkbox + drawn box).
import { computed, reactive, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { onClickOutside } from '@vueuse/core'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { usePopoverFocus } from '@/composables/usePopoverFocus'
import { Button, Input } from '@/components/ui'
import type { PrFilterOption, PrFilterOptions, PrFilters } from '@/lib/prInboxFilters'
import { Check, ListFilter } from 'lucide-vue-next'

export type PrFilterDimension = keyof PrFilters

const props = defineProps<{
  options: PrFilterOptions
  /** Stored selections (may include values no longer present — ignored). */
  modelValue: PrFilters
  /** Number of selected values that currently apply (badge on the button). */
  activeCount: number
  /** Display text for an option of a dimension. */
  optionLabel: (dimension: PrFilterDimension, option: PrFilterOption) => string
}>()

const emit = defineEmits<{
  update: [dimension: PrFilterDimension, values: string[]]
  clear: []
}>()

const { t } = useI18n()

const open = ref(false)
const wrap = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)
onClickOutside(wrap, () => { open.value = false })
useEscapeClose(open)
usePopoverFocus(open, wrap, panel)

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
  return {
    ...s,
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
  <div ref="wrap" class="relative shrink-0">
    <Button
      variant="outline"
      :class="open ? 'bg-accent text-foreground' : ''"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :title="t('prInbox.filters.button')"
      :aria-label="t('prInbox.filters.button')"
      @click="open = !open"
    >
      <ListFilter class="h-3.5 w-3.5" :stroke-width="1.75" />
      <span class="hidden @[30rem]/toolbar:inline">{{ t('prInbox.filters.button') }}</span>
      <span
        v-if="activeCount > 0"
        class="flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-medium text-primary-foreground"
      >
        {{ activeCount }}
      </span>
    </Button>

    <div
      v-if="open"
      ref="panel"
      role="dialog"
      :aria-label="t('prInbox.filters.button')"
      class="absolute right-0 top-full z-50 mt-2 flex max-h-[70vh] w-72 max-w-[calc(100vw-2rem)] flex-col rounded-lg border border-border/60 bg-popover shadow-xl shadow-black/30"
    >
      <div class="min-h-0 flex-1 overflow-y-auto p-3">
        <section v-for="s in sections" :key="s.key" class="mb-3 last:mb-0">
          <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ s.title }}</div>
          <p v-if="s.empty" class="px-1 text-[11px] text-muted-foreground/70">{{ t('prInbox.filters.noOptions') }}</p>
          <template v-else>
            <Input
              v-if="s.searchable"
              v-model="sectionSearch[s.key]"
              size="xs"
              class="mb-1.5"
              :placeholder="t('prInbox.filters.searchSection', { section: s.title })"
              :aria-label="t('prInbox.filters.searchSection', { section: s.title })"
            />
            <div class="max-h-40 overflow-y-auto" role="group" :aria-label="s.title">
              <label
                v-for="o in s.options"
                :key="o.value"
                class="flex items-center gap-2 rounded-md px-1.5 py-1 cursor-pointer hover:bg-accent/60 transition-colors has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-ring"
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
                <img v-if="o.avatarUrl" :src="o.avatarUrl" alt="" class="h-4 w-4 shrink-0 rounded-full bg-muted" />
                <span class="min-w-0 flex-1 truncate text-xs">{{ optionLabel(s.key, o) }}</span>
                <span class="shrink-0 text-[11px] text-muted-foreground tabular-nums">{{ o.count }}</span>
              </label>
              <p v-if="s.options.length === 0" class="px-1.5 py-1 text-[11px] text-muted-foreground/70">
                {{ t('prInbox.filters.noMatchSection') }}
              </p>
            </div>
          </template>
        </section>
      </div>
      <div class="flex items-center justify-end border-t border-border/60 px-3 py-2">
        <Button variant="ghost" size="xs" :disabled="activeCount === 0" @click="emit('clear')">
          {{ t('prInbox.filters.clearAll') }}
        </Button>
      </div>
    </div>
  </div>
</template>
