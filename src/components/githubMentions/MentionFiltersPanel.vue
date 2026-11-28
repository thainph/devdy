<script setup lang="ts">
// Filters of the GitHub Mentions settings modal (MentionOptionsMenu): one
// section per dimension (reason / account / repo), each a bordered checkbox
// list like Delivery Radar's members, with a search once it gets long.
import { computed, reactive } from 'vue'
import { useI18n } from 'vue-i18n'
import { Input } from '@/components/ui'
import type { MentionFilterDimension, MentionFilterOption, MentionFilters } from '@/lib/githubMentions'
import { Check } from 'lucide-vue-next'

const props = defineProps<{
  options: Record<MentionFilterDimension, MentionFilterOption[]>
  modelValue: MentionFilters
  optionLabel: (dimension: MentionFilterDimension, option: MentionFilterOption) => string
}>()

const emit = defineEmits<{
  update: [dimension: MentionFilterDimension, values: string[]]
}>()

const { t } = useI18n()

/** A list longer than this gets a search box. */
const SEARCH_AFTER = 8
const queries = reactive<Record<MentionFilterDimension, string>>({ reasons: '', accounts: '', repos: '' })

const sections = computed(() => ([
  { key: 'reasons', title: t('githubMentions.filters.reason') },
  { key: 'accounts', title: t('githubMentions.filters.account') },
  { key: 'repos', title: t('githubMentions.filters.repo') },
] as const).map((s) => {
  const all = props.options[s.key]
  const searchable = all.length > SEARCH_AFTER
  const q = searchable ? queries[s.key].trim().toLowerCase() : ''
  return {
    ...s,
    all,
    searchable,
    rows: q ? all.filter(o => props.optionLabel(s.key, o).toLowerCase().includes(q)) : all,
    selected: props.modelValue[s.key].filter(v => all.some(o => o.value === v)).length,
  }
}))

function isChecked(d: MentionFilterDimension, value: string) {
  return props.modelValue[d].includes(value)
}

function toggle(d: MentionFilterDimension, value: string) {
  const cur = props.modelValue[d]
  emit('update', d, cur.includes(value) ? cur.filter(v => v !== value) : [...cur, value])
}
</script>

<template>
  <section v-for="s in sections" :key="s.key" class="space-y-2">
    <h3 class="flex items-center text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
      {{ s.title }}
      <span v-if="s.selected > 0" class="ml-auto font-normal normal-case tracking-normal text-primary">{{ s.selected }}</span>
    </h3>
    <p v-if="s.all.length === 0" class="text-[11px] text-muted-foreground">{{ t('githubMentions.filters.noOptions') }}</p>
    <template v-else>
      <Input
        v-if="s.searchable"
        v-model="queries[s.key]"
        size="sm"
        class="w-full"
        :placeholder="t('githubMentions.filters.search')"
        :aria-label="`${s.title}: ${t('githubMentions.filters.search')}`"
      />
      <div class="max-h-48 overflow-auto rounded-md border border-border/70" role="group" :aria-label="s.title">
        <label
          v-for="o in s.rows"
          :key="o.value"
          class="flex cursor-pointer items-center gap-2.5 border-b border-border/40 px-3 py-1.5 transition-colors last:border-b-0 hover:bg-accent/60 has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-inset has-[input:focus-visible]:ring-ring"
        >
          <span
            class="flex h-4 w-4 shrink-0 items-center justify-center rounded border transition-colors"
            :class="isChecked(s.key, o.value) ? 'bg-primary border-primary text-white' : 'border-border'"
          >
            <Check v-if="isChecked(s.key, o.value)" class="h-3 w-3" :stroke-width="3" />
          </span>
          <input type="checkbox" class="sr-only" :checked="isChecked(s.key, o.value)" @change="toggle(s.key, o.value)">
          <span class="min-w-0 flex-1 truncate text-[13px]" :title="optionLabel(s.key, o)">{{ optionLabel(s.key, o) }}</span>
          <span class="shrink-0 text-[11px] tabular-nums text-muted-foreground">{{ o.count }}</span>
        </label>
        <p v-if="s.rows.length === 0" class="px-3 py-2 text-[11px] text-muted-foreground">{{ t('githubMentions.filters.noMatch') }}</p>
      </div>
    </template>
  </section>
</template>
