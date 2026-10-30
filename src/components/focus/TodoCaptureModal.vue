<script setup lang="ts">
// "+ Todo" dialog (spec §3.1) opened from a source — Slack thread, Gantt issue,
// RunView, PR Inbox card — or the Focus header. The form is prefilled from the
// context (title, project, source link, deadline) and nothing is saved until the
// user presses "Add todo" (⌘/Ctrl+Enter).
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Modal, Textarea } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { useProjectsStore } from '@/stores/projects'
import { draftToInput, emptyTodoDraft, type TodoDraft, type TodoPrefill } from '@/lib/focus'
import TodoMetaFields from './TodoMetaFields.vue'
import FocusAiSuggestButton from './FocusAiSuggestButton.vue'

const props = defineProps<{ open: boolean; prefill?: TodoPrefill | null }>()
const emit = defineEmits<{ close: []; saved: [id: string] }>()

const { t } = useI18n()
const { toast } = useToast()
const focus = useFocusStore()
const projects = useProjectsStore()

const draft = ref<TodoDraft>(emptyTodoDraft())
const saving = ref(false)
const textRef = ref<InstanceType<typeof Textarea> | null>(null)

const input = computed(() => draftToInput(draft.value, props.prefill?.runId ?? null))

watch(
  () => props.open,
  (open) => {
    if (!open) return
    const p = props.prefill ?? {}
    draft.value = emptyTodoDraft({
      text: p.text ?? '',
      projectId: p.projectId ?? '',
      sourceUrl: p.sourceUrl ?? '',
      dueDate: p.dueDate ?? '',
    })
    if (!projects.projects.length) void projects.fetchProjects()
    void nextTick(() => (textRef.value?.$el as HTMLElement | undefined)?.focus())
  },
  { immediate: true },
)

const projectId = computed({
  get: () => draft.value.projectId || null,
  set: (v: string | null) => (draft.value = { ...draft.value, projectId: v ?? '' }),
})

async function submit() {
  const args = input.value
  if (!args || saving.value) return
  saving.value = true
  try {
    const todo = await focus.add(args)
    toast.success(t('focus.toast.added'))
    emit('saved', todo.id)
    emit('close')
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) {
    e.preventDefault()
    void submit()
  }
}
</script>

<template>
  <Modal :open="open" :title="t('focus.capture.title')" size="md" :dismiss-on-overlay="false" @close="emit('close')">
    <div class="space-y-2.5 p-4" @keydown="onKeydown">
      <Textarea
        ref="textRef"
        v-model="draft.text"
        class="min-h-[4.5rem] w-full"
        :placeholder="t('focus.capture.textPlaceholder')"
      />
      <div class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.capture.project') }}</label>
        <ProjectSelect v-model="projectId" variant="default" size="sm" />
      </div>
      <TodoMetaFields v-model="draft">
        <template #suggest>
          <FocusAiSuggestButton v-model="draft" />
        </template>
      </TodoMetaFields>
    </div>
    <template #footer>
      <span class="mr-auto text-[11px] text-muted-foreground/70">⌘/Ctrl + Enter</span>
      <Button variant="ghost" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button :disabled="!input || saving" @click="submit">{{ t('focus.capture.save') }}</Button>
    </template>
  </Modal>
</template>
