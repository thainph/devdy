<script setup lang="ts">
// "Filtering: Overdue · icontruction            Clear filters" — the one line a
// list-detail sidebar shows under its search box while its filter menu narrows
// the list. Renders nothing when `parts` is empty.
import { useI18n } from 'vue-i18n'

defineProps<{
  /** Labels of the active filters, in menu order. */
  parts: string[]
}>()

const emit = defineEmits<{ clear: [] }>()

const { t } = useI18n()
</script>

<template>
  <div v-if="parts.length" class="flex min-w-0 shrink-0 items-center gap-1.5 px-3 pb-1.5 text-[11px] text-muted-foreground">
    <span class="shrink-0">{{ t('listPane.filtering') }}</span>
    <span class="min-w-0 truncate font-semibold text-primary" :title="parts.join(' · ')">{{ parts.join(' · ') }}</span>
    <button
      type="button"
      class="ml-auto shrink-0 cursor-pointer underline underline-offset-2 transition-colors hover:text-foreground"
      @click="emit('clear')"
    >
      {{ t('listPane.clearFilters') }}
    </button>
  </div>
</template>
