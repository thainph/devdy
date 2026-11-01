<script setup lang="ts">
/**
 * Create or edit one todo on the phone Controller — a full-height screen laid
 * out like the note editor: title, project, due date (with Today / Tomorrow /
 * Next week / No date chips) and a note. Focus fields (priority, estimate, AI
 * plan) stay desktop-only.
 *
 * Writes go through the `save` / `remove` props; the screen only goes back once
 * the Host accepted them, and stays (busy) meanwhile.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, X } from 'lucide-vue-next'
import { AppSelect, Banner, Button, Input, Textarea } from '@/components/ui'
import {
  activeDueChip,
  DUE_CHIPS,
  dueFromChip,
  joinTodoText,
  splitTodoText,
  type DueChip,
} from '@/lib/remoteTodos'
import ControllerHeader from './ControllerHeader.vue'
import HeaderButton from './HeaderButton.vue'
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
  error: string | null
  save: (input: TodoSave) => Promise<boolean>
  remove: (id: string) => Promise<boolean>
}>()

const emit = defineEmits<{
  back: []
  'clear-error': []
}>()

const { t } = useI18n()

const title = ref('')
const note = ref('')
const dueDate = ref('')
const projectId = ref('')
const confirmingDelete = ref(false)
const localError = ref<string | null>(null)

watch(
  () => props.todo,
  (todo) => {
    const parts = splitTodoText(todo?.text ?? '')
    title.value = parts.title
    note.value = parts.note
    dueDate.value = todo?.due_date ?? ''
    projectId.value = todo ? (todo.project_id ?? '') : props.defaultProjectId
    confirmingDelete.value = false
    localError.value = null
  },
  { immediate: true },
)

const projectOptions = computed(() => [
  { value: '', label: t('controller.data.noProject') },
  ...props.projects.map((p) => ({ value: p.id, label: p.name })),
])

const activeChip = computed(() => activeDueChip(dueDate.value, props.today))

function pickChip(chip: DueChip): void {
  dueDate.value = dueFromChip(chip, props.today)
}

async function submit(): Promise<void> {
  if (props.busy) return
  if (!title.value.trim()) {
    localError.value = t('controller.todo.sheet.needsTitle')
    return
  }
  localError.value = null
  const ok = await props.save({
    id: props.todo?.id ?? null,
    text: joinTodoText(title.value, note.value),
    dueDate: dueDate.value,
    projectId: projectId.value,
    base: props.todo,
  })
  if (ok) emit('back')
}

/** Two taps instead of a modal. */
async function onDelete(): Promise<void> {
  if (!props.todo || props.busy) return
  if (!confirmingDelete.value) {
    confirmingDelete.value = true
    return
  }
  if (await props.remove(props.todo.id)) emit('back')
}

const banner = computed(() => localError.value ?? props.error)
function clearBanner(): void {
  localError.value = null
  emit('clear-error')
}
</script>

<template>
  <div class="relative flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="todo ? t('controller.todo.sheet.editTitle') : t('controller.todo.sheet.newTitle')">
      <template #leading>
        <HeaderButton class="-ml-1.5" :label="t('controller.data.back')" @click="emit('back')">
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </HeaderButton>
      </template>
    </ControllerHeader>

    <Banner
      v-if="banner"
      tone="destructive"
      class="shrink-0 gap-2 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{ banner }}
      <template #actions>
        <button
          class="shrink-0 cursor-pointer text-destructive"
          :title="t('controller.data.dismiss')"
          :aria-label="t('controller.data.dismiss')"
          @click="clearBanner"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </template>
    </Banner>

    <main class="flex-1 min-h-0 overflow-auto px-3 py-3">
      <form class="flex min-h-full flex-col" @submit.prevent="submit">
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

        <div class="mb-3">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.data.project') }}</span>
          <AppSelect v-model="projectId" size="md" class="w-full" :options="projectOptions" />
        </div>

        <label class="mb-2 block">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.due') }}</span>
          <Input v-model="dueDate" type="date" size="md" class="w-full" />
        </label>
        <div class="mb-3 flex flex-wrap gap-1.5">
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

        <label class="mb-3 flex flex-1 flex-col">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.todo.sheet.note') }}</span>
          <Textarea
            v-model="note"
            size="md"
            class="min-h-[10rem] w-full flex-1 resize-none"
            :placeholder="t('controller.todo.sheet.notePlaceholder')"
          />
        </label>

        <div class="grid grid-cols-2 gap-2 pb-[env(safe-area-inset-bottom)]">
          <Button v-if="todo" variant="destructive" size="md" :disabled="busy" @click="onDelete">
            {{ confirmingDelete ? t('controller.todo.sheet.confirmDelete') : t('controller.todo.sheet.delete') }}
          </Button>
          <Button v-else variant="outline" size="md" :disabled="busy" @click="emit('back')">
            {{ t('controller.data.cancel') }}
          </Button>
          <Button type="submit" size="md" :disabled="busy">
            {{ busy ? t('controller.data.saving') : t('controller.data.save') }}
          </Button>
        </div>
      </form>
    </main>
  </div>
</template>
