<script setup lang="ts">
/**
 * Bottom sheet to create or edit a todo on the phone: title, note, due date (with
 * Today / Tomorrow / Next week / No date chips) and project. Focus fields
 * (priority, estimate, AI plan) stay desktop-only.
 *
 * The parent decides when it closes: `save` / `delete` carry the edit out and
 * the sheet stays open (busy) until the parent hears back from the Host.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Input, Textarea, AppSelect } from '@/components/ui'
import {
  activeDueChip,
  DUE_CHIPS,
  dueFromChip,
  joinTodoText,
  splitTodoText,
  type DueChip,
} from '@/lib/remoteTodos'
import type { ProjectInfo, TodoInfo } from '../protocol'
import type { TodoSave } from '../dataStore'

const props = defineProps<{
  /** The todo being edited; null for a new one. */
  todo: TodoInfo | null
  projects: ProjectInfo[]
  /** Project a new todo starts in ('' = none). */
  defaultProjectId: string
  /** Today as local `YYYY-MM-DD`. */
  today: string
  busy: boolean
}>()

const emit = defineEmits<{
  save: [input: TodoSave]
  delete: [id: string]
  close: []
}>()

const { t } = useI18n()

const title = ref('')
const note = ref('')
const dueDate = ref('')
const projectId = ref('')
const confirmingDelete = ref(false)

watch(
  () => props.todo,
  (todo) => {
    const parts = splitTodoText(todo?.text ?? '')
    title.value = parts.title
    note.value = parts.note
    dueDate.value = todo?.due_date ?? ''
    projectId.value = todo ? (todo.project_id ?? '') : props.defaultProjectId
    confirmingDelete.value = false
  },
  { immediate: true },
)

const projectOptions = computed(() => [
  { value: '', label: t('controller.data.noProject') },
  ...props.projects.map((p) => ({ value: p.id, label: p.name })),
])

const activeChip = computed(() => activeDueChip(dueDate.value, props.today))
const canSave = computed(() => title.value.trim().length > 0 && !props.busy)

function pickChip(chip: DueChip): void {
  dueDate.value = dueFromChip(chip, props.today)
}

function save(): void {
  if (!canSave.value) return
  emit('save', {
    id: props.todo?.id ?? null,
    text: joinTodoText(title.value, note.value),
    dueDate: dueDate.value,
    projectId: projectId.value,
  })
}

/** Two taps instead of a modal: the sheet already is the modal. */
function remove(): void {
  if (!props.todo) return
  if (!confirmingDelete.value) {
    confirmingDelete.value = true
    return
  }
  emit('delete', props.todo.id)
}
</script>

<template>
  <div class="absolute inset-0 z-20 flex flex-col justify-end">
    <div class="absolute inset-0 bg-black/50" @click="emit('close')" />
    <form
      class="relative rounded-t-2xl border-t border-border bg-card px-4 pt-2.5 pb-[max(1.25rem,env(safe-area-inset-bottom))] shadow-2xl"
      @submit.prevent="save"
    >
      <div class="mx-auto mb-3 h-1.5 w-10 rounded-full bg-foreground/20" />
      <p class="mb-3 text-sm font-semibold">
        {{ todo ? t('controller.todo.sheet.editTitle') : t('controller.todo.sheet.newTitle') }}
      </p>

      <label class="mb-3 block">
        <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.title') }}</span>
        <Input
          v-model="title"
          size="md"
          class="w-full"
          autocomplete="off"
          enterkeyhint="done"
          :placeholder="t('controller.todo.sheet.titlePlaceholder')"
        />
      </label>

      <label class="mb-3 block">
        <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.note') }}</span>
        <Textarea
          v-model="note"
          size="md"
          :rows="3"
          class="w-full resize-none"
          :placeholder="t('controller.todo.sheet.notePlaceholder')"
        />
      </label>

      <div class="mb-2 grid grid-cols-2 gap-2.5">
        <label class="block min-w-0">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.due') }}</span>
          <Input v-model="dueDate" type="date" size="md" class="w-full" />
        </label>
        <div class="min-w-0">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.project') }}</span>
          <AppSelect v-model="projectId" size="md" class="w-full" :options="projectOptions" />
        </div>
      </div>

      <div class="mb-4 flex flex-wrap gap-1.5">
        <button
          v-for="chip in DUE_CHIPS"
          :key="chip"
          type="button"
          class="rounded-full border px-2.5 py-1 text-xs transition-colors cursor-pointer"
          :class="
            activeChip === chip
              ? 'border-primary bg-primary/15 text-foreground'
              : 'border-border bg-secondary text-muted-foreground hover:text-foreground'
          "
          @click="pickChip(chip)"
        >
          {{ t(`controller.todo.chips.${chip}`) }}
        </button>
      </div>

      <div class="grid grid-cols-2 gap-2">
        <Button v-if="todo" variant="destructive" size="md" :disabled="busy" @click="remove">
          {{ confirmingDelete ? t('controller.todo.sheet.confirmDelete') : t('controller.todo.sheet.delete') }}
        </Button>
        <Button v-else variant="outline" size="md" :disabled="busy" @click="emit('close')">
          {{ t('controller.data.cancel') }}
        </Button>
        <Button type="submit" size="md" :disabled="!canSave">
          {{ busy ? t('controller.data.saving') : t('controller.data.save') }}
        </Button>
      </div>
    </form>
  </div>
</template>
