<script setup lang="ts">
// "Filters" header button + popover for GitHub Mentions: reason / account /
// repo as checkbox lists. Same panel and rows as PrInboxFiltersPopover.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { onClickOutside } from '@vueuse/core'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { usePopoverFocus } from '@/composables/usePopoverFocus'
import { Button } from '@/components/ui'
import type { MentionFilterDimension, MentionFilterOption, MentionFilters } from '@/lib/githubMentions'
import { Check, ListFilter } from 'lucide-vue-next'

const props = defineProps<{
  options: Record<MentionFilterDimension, MentionFilterOption[]>
  modelValue: MentionFilters
  activeCount: number
  optionLabel: (dimension: MentionFilterDimension, option: MentionFilterOption) => string
}>()

const emit = defineEmits<{
  update: [dimension: MentionFilterDimension, values: string[]]
  clear: []
}>()

const { t } = useI18n()
const open = ref(false)
const wrap = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)
onClickOutside(wrap, () => { open.value = false })
useEscapeClose(open)
usePopoverFocus(open, wrap, panel)

const sections = computed(() => ([
  { key: 'reasons', title: t('githubMentions.filters.reason') },
  { key: 'accounts', title: t('githubMentions.filters.account') },
  { key: 'repos', title: t('githubMentions.filters.repo') },
] as const).map(s => ({ ...s, options: props.options[s.key] })))

function isChecked(d: MentionFilterDimension, value: string) {
  return props.modelValue[d].includes(value)
}

function toggle(d: MentionFilterDimension, value: string) {
  const cur = props.modelValue[d]
  emit('update', d, cur.includes(value) ? cur.filter(v => v !== value) : [...cur, value])
}
</script>

<template>
  <div ref="wrap" class="relative shrink-0">
    <Button
      variant="outline"
      :class="open ? 'bg-accent text-foreground' : ''"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :title="t('githubMentions.filters.button')"
      :aria-label="t('githubMentions.filters.button')"
      @click="open = !open"
    >
      <ListFilter class="h-3.5 w-3.5" :stroke-width="1.75" />
      <span class="hidden lg:inline">{{ t('githubMentions.filters.button') }}</span>
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
      :aria-label="t('githubMentions.filters.button')"
      class="absolute right-0 top-full z-50 mt-2 flex max-h-[70vh] w-72 max-w-[calc(100vw-2rem)] flex-col rounded-lg border border-border/60 bg-popover shadow-xl shadow-black/30"
    >
      <div class="min-h-0 flex-1 overflow-y-auto p-3">
        <section v-for="s in sections" :key="s.key" class="mb-3 last:mb-0">
          <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ s.title }}</div>
          <p v-if="s.options.length === 0" class="px-1 text-[11px] text-muted-foreground/70">{{ t('githubMentions.filters.noOptions') }}</p>
          <div v-else class="max-h-40 overflow-y-auto" role="group" :aria-label="s.title">
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
              <input type="checkbox" class="sr-only" :checked="isChecked(s.key, o.value)" @change="toggle(s.key, o.value)">
              <span class="min-w-0 flex-1 truncate text-xs">{{ optionLabel(s.key, o) }}</span>
              <span class="shrink-0 text-[11px] text-muted-foreground tabular-nums">{{ o.count }}</span>
            </label>
          </div>
        </section>
      </div>
      <div class="flex items-center justify-end border-t border-border/60 px-3 py-2">
        <Button variant="ghost" size="xs" :disabled="activeCount === 0" @click="emit('clear')">
          {{ t('githubMentions.filters.clearAll') }}
        </Button>
      </div>
    </div>
  </div>
</template>
