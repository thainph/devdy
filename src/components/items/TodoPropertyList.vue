<script setup lang="ts">
// The new todo's properties as a labelled list (the Todos screen's create pane,
// components/items/ItemCreatePane): priority (segmented), estimate (`30m`,
// `2h`, `1h30` — lib/focus parseEstimate), the deadline's time (only once a
// deadline is picked; the date itself is in the header) and the source link
// as a removable chip, or "+ Paste a GitHub / Slack link". The host owns the
// draft (useQuickCapture `todoMeta`).
import { computed, nextTick, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Clock, Flag, Link2, Timer, X } from 'lucide-vue-next'
import { Button, Input, SegmentedControl } from '@/components/ui'
import { guessSourceType, parseEstimate, sourceShortLabel, type TodoDraft, type TodoPriority } from '@/lib/focus'
import { SOURCE_ICON } from '@/components/focus/sourceIcon'

const draft = defineModel<TodoDraft>({ required: true })

const { t } = useI18n()

const PRIORITIES = computed(() =>
  ([1, 2, 3] as TodoPriority[]).map((p) => ({ value: p, label: `P${p}`, title: t(`focus.priority.${p}`) })),
)

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

// "+ Paste a link": an inline field; Enter / leaving it keeps a non-empty value.
const addingSource = ref(false)
const sourceInput = ref('')
const sourceRef = ref<InstanceType<typeof Input> | null>(null)
async function startSource() {
  sourceInput.value = ''
  addingSource.value = true
  await nextTick()
  ;(sourceRef.value?.$el as HTMLInputElement | undefined)?.focus()
}
function commitSource() {
  if (!addingSource.value) return
  addingSource.value = false
  const url = sourceInput.value.trim()
  if (url) set('sourceUrl', url)
}
function onSourceKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.metaKey && !e.ctrlKey && !e.isComposing) {
    e.preventDefault()
    commitSource()
  } else if (e.key === 'Escape') {
    // Only closes the field, not the whole form.
    e.preventDefault()
    e.stopPropagation()
    addingSource.value = false
  }
}

const keyClass = 'flex items-center gap-1.5 text-xs text-muted-foreground'
</script>

<template>
  <div class="grid max-w-xl grid-cols-[7rem_minmax(0,1fr)] items-center gap-x-3 gap-y-2">
    <span :class="keyClass"><Flag class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />{{ t('item.create.priority') }}</span>
    <SegmentedControl
      :model-value="draft.priority"
      :options="PRIORITIES"
      :stretch="false"
      class="w-max"
      :aria-label="t('item.create.priority')"
      @update:model-value="set('priority', $event)"
    />

    <span :class="keyClass"><Timer class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />{{ t('item.create.estimate') }}</span>
    <div class="min-w-0">
      <Input
        :model-value="draft.estimate"
        size="sm"
        class="w-40"
        :class="estimateInvalid ? 'border-destructive focus-visible:ring-destructive' : ''"
        :placeholder="t('focus.drawer.estimatePlaceholder')"
        :aria-label="t('item.create.estimate')"
        :aria-invalid="estimateInvalid"
        @update:model-value="set('estimate', $event)"
      />
      <p v-if="estimateInvalid" class="mt-1 text-[11px] text-destructive">{{ t('focus.drawer.estimateInvalid') }}</p>
    </div>

    <span :class="keyClass"><Clock class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />{{ t('item.create.time') }}</span>
    <Input
      :model-value="draft.dueTime"
      type="time"
      size="sm"
      class="w-40"
      :disabled="!draft.dueDate"
      :title="draft.dueDate ? undefined : t('item.create.timeNeedsDue')"
      :aria-label="t('item.create.time')"
      @update:model-value="set('dueTime', $event)"
    />

    <span :class="keyClass"><Link2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />{{ t('item.create.source') }}</span>
    <div class="flex min-h-8 min-w-0 items-center">
      <span
        v-if="draft.sourceUrl"
        class="inline-flex h-7 min-w-0 max-w-full items-center gap-1.5 rounded-full border border-border bg-muted/40 pl-2.5 pr-0.5 text-xs"
        :title="draft.sourceUrl"
      >
        <component :is="SOURCE_ICON[sourceType ?? 'url']" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="min-w-0 truncate">{{ sourceLabel }}</span>
        <Button
          variant="ghost"
          size="icon-xs"
          class="shrink-0 rounded-full"
          :title="t('focus.capture.removeSource')"
          :aria-label="t('focus.capture.removeSource')"
          @click="set('sourceUrl', '')"
        >
          <X class="h-3 w-3" :stroke-width="2" />
        </Button>
      </span>
      <Input
        v-else-if="addingSource"
        ref="sourceRef"
        v-model="sourceInput"
        size="sm"
        class="w-full max-w-sm"
        :placeholder="t('item.create.sourcePlaceholder')"
        :aria-label="t('item.create.source')"
        @keydown="onSourceKeydown"
        @blur="commitSource"
      />
      <button
        v-else
        type="button"
        class="cursor-pointer text-xs text-muted-foreground transition-colors hover:text-foreground"
        @click="startSource"
      >
        {{ t('item.create.addSource') }}
      </button>
    </div>
  </div>
</template>
