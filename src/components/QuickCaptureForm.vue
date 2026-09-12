<script setup lang="ts">
// The Todo/Note capture form itself — deliberately UI-shell-free so the exact
// same fields, shortcuts and save behaviour back BOTH capture surfaces:
//   • QuickCapturePanel — the in-app slide-over (no route change, no lost context)
//   • QuickCreateWindow — the standalone always-on-top OS window (second monitor)
//
// The draft it edits lives in useQuickCapture, not here, so closing the surface
// doesn't discard a half-written capture. Saving writes straight to the shared
// SQLite DB via the stores and emits `saved` so the host can react.
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Input, Textarea, AppSelect } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useTodosStore } from '@/stores/todos'
import { useNotesStore } from '@/stores/notes'
import { useProjectsStore } from '@/stores/projects'
import { useQuickCapture, type QuickCaptureTab } from '@/composables/useQuickCapture'

const props = withDefaults(defineProps<{ autofocus?: boolean }>(), { autofocus: true })

const emit = defineEmits<{
  saved: [tab: QuickCaptureTab]
  /** Fired on Esc so the host can dismiss itself. */
  dismiss: []
}>()

const { t } = useI18n()
const { toast } = useToast()
const todos = useTodosStore()
const notes = useNotesStore()
const projects = useProjectsStore()
const {
  tab,
  todoText,
  noteTitle,
  noteContent,
  projectId,
  contextProjectId,
  runId,
  clearDraft,
} = useQuickCapture()

const NO_PROJECT = ''

const saving = ref(false)
const titleRef = ref<InstanceType<typeof Input> | null>(null)
const todoRef = ref<InstanceType<typeof Textarea> | null>(null)
const noteBodyRef = ref<InstanceType<typeof Textarea> | null>(null)

const projectOptions = computed(() => [
  { value: NO_PROJECT, label: t('todos.quick.noProject') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })),
])

// The run backlink only survives while the capture stays on the project it came
// from — re-pointing it at another project would make the link nonsense.
const effectiveRunId = computed(() =>
  runId.value && projectId.value && projectId.value === contextProjectId.value
    ? runId.value
    : null,
)

const canSubmit = computed(() =>
  tab.value === 'todo'
    ? todoText.value.trim().length > 0
    : noteTitle.value.trim().length > 0 || noteContent.value.trim().length > 0,
)

function focusFirstField() {
  if (!props.autofocus) return
  nextTick(() => {
    const el =
      tab.value === 'todo'
        ? todoRef.value?.$el
        : // With a title already filled in (e.g. a captured selection) the body
          // is where the user actually wants to land.
          noteTitle.value
          ? noteBodyRef.value?.$el
          : titleRef.value?.$el
    ;(el as HTMLElement | undefined)?.focus()
  })
}

async function submit() {
  if (!canSubmit.value || saving.value) return
  saving.value = true
  const savedTab = tab.value
  const project = projectId.value || null
  try {
    if (savedTab === 'todo') {
      await todos.add(todoText.value, project, effectiveRunId.value)
      toast.success(t('todos.quick.todoAdded'))
    } else {
      await notes.add(noteTitle.value, noteContent.value, project, effectiveRunId.value)
      toast.success(t('todos.quick.noteSaved'))
    }
    clearDraft(savedTab)
    emit('saved', savedTab)
    focusFirstField()
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

// ⌘/Ctrl+Enter saves; Esc asks the host to dismiss. Plain Enter stays a newline
// (markdown checklists and multi-line notes are the common case).
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) {
    e.preventDefault()
    submit()
  } else if (e.key === 'Escape') {
    e.preventDefault()
    emit('dismiss')
  }
}

watch(tab, focusFirstField)

onMounted(() => {
  projects.fetchProjects()
  focusFirstField()
})

defineExpose({ submit, canSubmit, saving })
</script>

<template>
  <div class="space-y-2.5" @keydown="onKeydown">
    <!-- Todo -->
    <template v-if="tab === 'todo'">
      <Textarea
        ref="todoRef"
        v-model="todoText"
        rows="6"
        :placeholder="t('todos.quick.todoPlaceholder')"
      />
    </template>

    <!-- Note -->
    <template v-else>
      <Input ref="titleRef" v-model="noteTitle" :placeholder="t('todos.quick.noteTitlePlaceholder')" />
      <Textarea
        ref="noteBodyRef"
        v-model="noteContent"
        rows="8"
        :placeholder="t('todos.quick.notePlaceholder')"
      />
    </template>

    <!-- Project link (pre-selected from the capture context) -->
    <div class="space-y-1">
      <label class="block text-[11px] font-medium text-muted-foreground">
        {{ t('todos.quick.projectLabel') }}
      </label>
      <AppSelect
        v-model="projectId"
        :options="projectOptions"
        :placeholder="t('todos.quick.noProject')"
      />
      <p v-if="effectiveRunId" class="text-[11px] text-muted-foreground/80">
        {{ t('todos.quick.runLinked') }}
      </p>
    </div>
  </div>
</template>
