<script setup lang="ts">
// The "+ Todo" fields beyond title and project (spec §3.1): deadline (date + optional
// time), priority, estimate (`30m`, `2h`, `1h30`) and the source the todo was
// created from, shown as a removable chip. Shared by QuickCaptureForm (item window)
// and TodoCaptureModal (the "+ Todo" buttons). The host owns the draft.
//
// The `suggest` slot is where P3's "✨ Suggest" (AI fills the form) goes; nothing
// renders there yet.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { X } from 'lucide-vue-next'
import { DatePicker, Input } from '@/components/ui'
import { guessSourceType, parseEstimate, sourceShortLabel, type TodoDraft, type TodoPriority } from '@/lib/focus'
import { SOURCE_ICON } from './sourceIcon'

const draft = defineModel<TodoDraft>({ required: true })

const { t } = useI18n()

const PRIORITIES: TodoPriority[] = [1, 2, 3]

const estimateInvalid = computed(() => parseEstimate(draft.value.estimate) === undefined)

const sourceType = computed(() => guessSourceType(draft.value.sourceUrl))
const sourceLabel = computed(() => {
  const url = draft.value.sourceUrl
  const short = sourceShortLabel(sourceType.value, null, url)
  if (short) return short
  if (sourceType.value && sourceType.value !== 'url') return t(`focus.sources.${sourceType.value}`)
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
})

function set<K extends keyof TodoDraft>(key: K, value: TodoDraft[K]) {
  draft.value = { ...draft.value, [key]: value }
}
</script>

<template>
  <div class="space-y-2.5">
    <div class="grid grid-cols-2 gap-2.5">
      <!-- Deadline: date (× clears it, and the time with it) + optional time -->
      <div class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.capture.due') }}</label>
        <div class="flex items-center">
          <DatePicker
            :model-value="draft.dueDate"
            size="sm"
            class="min-w-0 flex-1"
            :class="draft.dueDate ? 'rounded-r-none' : ''"
            :placeholder="t('focus.capture.due')"
            @update:model-value="set('dueDate', $event)"
          />
          <button
            v-if="draft.dueDate"
            type="button"
            class="flex h-8 w-7 shrink-0 cursor-pointer items-center justify-center rounded-r-md border border-l-0 border-border bg-background text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            :title="t('focus.drawer.clearDue')"
            :aria-label="t('focus.drawer.clearDue')"
            @click="draft = { ...draft, dueDate: '', dueTime: '' }"
          >
            <X class="h-3 w-3" :stroke-width="2" />
          </button>
        </div>
      </div>
      <div class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.capture.dueTime') }}</label>
        <Input
          :model-value="draft.dueTime"
          type="time"
          size="sm"
          class="w-full"
          :disabled="!draft.dueDate"
          @update:model-value="set('dueTime', $event)"
        />
      </div>
    </div>

    <div class="grid grid-cols-2 gap-2.5">
      <div class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.capture.priority') }}</label>
        <div class="flex h-8 overflow-hidden rounded-md border border-border" role="radiogroup" :aria-label="t('focus.capture.priority')">
          <button
            v-for="p in PRIORITIES"
            :key="p"
            type="button"
            role="radio"
            :aria-checked="draft.priority === p"
            :title="t(`focus.priority.${p}`)"
            class="flex-1 cursor-pointer text-xs font-medium transition-colors not-first:border-l not-first:border-border"
            :class="draft.priority === p
              ? p === 1 ? 'bg-red-600 text-white' : 'bg-primary text-primary-foreground'
              : 'bg-background text-muted-foreground hover:bg-accent hover:text-foreground'"
            @click="set('priority', p)"
          >
            P{{ p }}
          </button>
        </div>
      </div>
      <div class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.capture.estimate') }}</label>
        <Input
          :model-value="draft.estimate"
          size="sm"
          class="w-full"
          :class="estimateInvalid ? 'border-destructive focus-visible:ring-destructive' : ''"
          :placeholder="t('focus.drawer.estimatePlaceholder')"
          :aria-invalid="estimateInvalid"
          @update:model-value="set('estimate', $event)"
        />
      </div>
    </div>
    <p v-if="estimateInvalid" class="text-[11px] text-destructive">{{ t('focus.drawer.estimateInvalid') }}</p>

    <div v-if="draft.sourceUrl || $slots.suggest" class="flex items-center gap-2">
      <span v-if="draft.sourceUrl" class="text-[11px] font-medium text-muted-foreground">{{ t('focus.capture.source') }}</span>
      <span
        v-if="draft.sourceUrl"
        class="inline-flex min-w-0 items-center gap-1 rounded-full border border-border bg-muted/40 py-0.5 pl-2 pr-1 text-[11px]"
        :title="draft.sourceUrl"
      >
        <component :is="SOURCE_ICON[sourceType ?? 'url']" class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="max-w-48 truncate">{{ sourceLabel }}</span>
        <button
          type="button"
          class="flex h-4 w-4 cursor-pointer items-center justify-center rounded-full text-muted-foreground hover:bg-accent hover:text-foreground"
          :title="t('focus.capture.removeSource')"
          :aria-label="t('focus.capture.removeSource')"
          @click="set('sourceUrl', '')"
        >
          <X class="h-2.5 w-2.5" :stroke-width="2" />
        </button>
      </span>
      <slot name="suggest" />
    </div>
  </div>
</template>
