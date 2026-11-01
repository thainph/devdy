<script setup lang="ts">
// A todo's properties at the top of the detail body: its project and its
// deadline (× clears it). Same label-over-field look as TodoMetaFields.
import { useI18n } from 'vue-i18n'
import { X } from 'lucide-vue-next'
import { DatePicker } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import type { Todo } from '@/stores/todos'

defineProps<{ todo: Todo }>()

const emit = defineEmits<{
  setProject: [projectId: string | null]
  setDueDate: [dueDate: string | null]
}>()

const { t } = useI18n()
</script>

<template>
  <div class="shrink-0 border-b border-border/60 px-5 py-3">
    <div class="grid max-w-xl grid-cols-2 gap-2.5">
      <div class="min-w-0 space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('item.fieldProject') }}</label>
        <ProjectSelect
          :model-value="todo.project_id"
          variant="outline"
          size="sm"
          @update:model-value="emit('setProject', $event)"
        />
      </div>
      <div class="min-w-0 space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('item.fieldDue') }}</label>
        <div class="flex items-center">
          <DatePicker
            :model-value="todo.due_date ?? ''"
            size="sm"
            class="min-w-0 flex-1"
            :class="todo.due_date ? 'rounded-r-none' : ''"
            :placeholder="t('item.setDueDate')"
            @update:model-value="emit('setDueDate', $event || null)"
          />
          <button
            v-if="todo.due_date"
            type="button"
            class="flex h-8 w-7 shrink-0 cursor-pointer items-center justify-center rounded-r-md border border-l-0 border-border bg-background text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            :title="t('item.clearDueDate')"
            :aria-label="t('item.clearDueDate')"
            @click="emit('setDueDate', null)"
          >
            <X class="h-3 w-3" :stroke-width="2" />
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
