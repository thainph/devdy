<script setup lang="ts">
/**
 * "How long did it take?" after Done on a started, estimated todo (BR-11,
 * spec §3.3) — a bottom sheet prefilled with the Host's `focus_actual`
 * suggestion once it arrives; Skip leaves `actual_min` unset.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2 } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { formatEstimate, type FocusTodo } from '@/lib/focus'
import { parseActual } from '@/lib/remoteFocus'
import { splitTodoText } from '@/lib/remoteTodos'

const props = defineProps<{
  todo: FocusTodo
  /** The Host's suggestion: undefined while asked, null when it has none. */
  suggestion: number | null | undefined
}>()

const emit = defineEmits<{
  save: [minutes: number]
  close: []
}>()

const { t } = useI18n()

const value = ref('')
const touched = ref(false)
watch(
  () => props.suggestion,
  (m) => {
    // Never overwrite what the user started typing.
    if (!touched.value && m) value.value = formatEstimate(m)
  },
  { immediate: true },
)
// Actual time has a wider range than an estimate (BR-01: 1 min – 10 days).
const minutes = computed(() => parseActual(value.value))

function submit(): void {
  if (minutes.value != null) emit('save', minutes.value)
}
</script>

<template>
  <div class="absolute inset-0 z-30 flex flex-col justify-end">
    <div class="ui-backdrop absolute inset-0 bg-black/50" @click="emit('close')" />
    <form
      class="ctl-sheet relative flex flex-col rounded-t-2xl border-t border-border bg-card px-4 pt-2.5 pb-[max(1rem,env(safe-area-inset-bottom))] shadow-2xl"
      @submit.prevent="submit"
    >
      <div class="mx-auto mb-2 h-1.5 w-10 shrink-0 rounded-full bg-foreground/20" />
      <p class="text-sm font-semibold">{{ t('focus.actual.title') }}</p>
      <p class="mt-0.5 truncate text-xs text-muted-foreground">{{ splitTodoText(todo.text).title }}</p>
      <p class="mt-2 text-xs text-muted-foreground">
        {{ t('focus.actual.hint', { estimate: formatEstimate(todo.estimate_min) }) }}
      </p>
      <div class="mt-3 flex items-center gap-2">
        <Input
          v-model="value"
          size="md"
          class="min-w-0 flex-1"
          autocomplete="off"
          enterkeyhint="done"
          :class="minutes === undefined ? 'border-destructive' : ''"
          :placeholder="t('focus.actual.placeholder')"
          @input="touched = true"
        />
        <Loader2 v-if="suggestion === undefined" class="h-4 w-4 shrink-0 animate-spin text-muted-foreground" :stroke-width="2" />
      </div>
      <p v-if="minutes === undefined" class="mt-1 text-[11px] text-destructive">{{ t('focus.drawer.estimateInvalid') }}</p>
      <div class="mt-3 grid grid-cols-2 gap-2">
        <Button variant="outline" size="md" @click="emit('close')">{{ t('focus.actual.skip') }}</Button>
        <Button type="submit" size="md" :disabled="minutes == null">{{ t('focus.actual.save') }}</Button>
      </div>
    </form>
  </div>
</template>
