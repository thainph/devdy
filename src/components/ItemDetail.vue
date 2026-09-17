<script setup lang="ts">
// The one and only rendering of a single EXISTING todo / note — both how it
// reads and how it is edited, plus the save itself.
//
// Its only host today is ItemWindow (the standalone todo / note window), which
// owns the chrome — titlebar, pin, preview toggle, delete, save — and drives
// this component through `editing` plus the exposed `save()` / `reset()`. The
// split is kept because the fields and their persistence are the part worth
// protecting: "title + body + project, ⌘⏎ saves, then update() and maybe
// setProject()" used to exist in three places and drifted between them.
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { FolderOpen, MessageSquare } from 'lucide-vue-next'
import { AppSelect, Input, Textarea } from '@/components/ui'
import { useMarkdown } from '@/lib/markdown'
import { useNotesStore, type Note } from '@/stores/notes'
import { useTodosStore, type Todo } from '@/stores/todos'
import { useProjectsStore } from '@/stores/projects'

const NO_PROJECT = ''

const props = withDefaults(
  defineProps<{
    kind: 'todo' | 'note'
    item: Note | Todo | null
    /** false renders the item as it reads; true renders the fields. */
    editing: boolean
    /** Offer the "open the AI session this came from" link (view mode). */
    showRunLink?: boolean
    /** Esc asks the host to cancel. Off where Esc must not discard work. */
    escCancels?: boolean
    autofocus?: boolean
  }>(),
  { showRunLink: true, escCancels: true, autofocus: true },
)

const emit = defineEmits<{
  /** Written to the DB; the host decides what to close / notify. */
  saved: []
  /** Esc in edit mode (only when `escCancels`). */
  cancel: []
  openRun: []
}>()

const { t } = useI18n()
const notes = useNotesStore()
const todos = useTodosStore()
const projects = useProjectsStore()
const { renderText, loadMarkdown } = useMarkdown()

const saving = ref(false)

// Draft fields. `title` is note-only; `content` is the note body or the todo
// text, so one set of fields covers both kinds.
const title = ref('')
const content = ref('')
const projectId = ref<string>(NO_PROJECT)

const titleRef = ref<InstanceType<typeof Input> | null>(null)
const contentRef = ref<InstanceType<typeof Textarea> | null>(null)

const isNote = computed(() => props.kind === 'note')

// What is currently stored — the yardstick for both `dirty` and `reset`.
const storedTitle = computed(() => (isNote.value ? (props.item as Note | null)?.title ?? '' : ''))
const storedContent = computed(() =>
  isNote.value ? (props.item as Note | null)?.content ?? '' : (props.item as Todo | null)?.text ?? '',
)
const storedProject = computed(() => props.item?.project_id ?? NO_PROJECT)
const done = computed(() => (isNote.value ? false : !!(props.item as Todo | null)?.done))

const projectOptions = computed(() => [
  { value: NO_PROJECT, label: t('item.noProject') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })),
])

const projectName = computed(
  () => projects.projects.find((p) => p.id === props.item?.project_id)?.name ?? null,
)

const dirty = computed(
  () =>
    title.value !== storedTitle.value ||
    content.value !== storedContent.value ||
    projectId.value !== storedProject.value,
)

// A note may lose its title or its body, but not both; a todo must keep text —
// saving it empty would delete the row (see the todos store), and Delete is the
// only thing allowed to do that.
const canSave = computed(() =>
  isNote.value ? !!(title.value.trim() || content.value.trim()) : content.value.trim().length > 0,
)

/** Reload the fields from what is stored, dropping any unsaved edits. */
function reset() {
  title.value = storedTitle.value
  content.value = storedContent.value
  projectId.value = storedProject.value
}

/** The current fields, for handing the edit over to another surface. */
function draft() {
  return { title: title.value, content: content.value, projectId: projectId.value || null }
}

/** Take over fields handed in from elsewhere (e.g. a pre-filled capture). */
function applyDraft(draft: { title?: string; content?: string; projectId?: string | null }) {
  if (isNote.value && draft.title !== undefined) title.value = draft.title
  if (draft.content !== undefined) content.value = draft.content
  if (draft.projectId !== undefined) projectId.value = draft.projectId ?? NO_PROJECT
}

function focus() {
  if (!props.autofocus) return
  nextTick(() => {
    // With a title already there, the body is where the user wants to land.
    const el = isNote.value && !title.value ? titleRef.value?.$el : contentRef.value?.$el
    ;(el as HTMLElement | undefined)?.focus()
  })
}

async function save() {
  if (!props.item || !canSave.value || saving.value) return
  saving.value = true
  const id = props.item.id
  const nextProject = projectId.value || null
  try {
    if (isNote.value) await notes.update(id, title.value, content.value)
    else await todos.update(id, content.value)
    // Only touch the link when it actually moved: setProject also clears the run
    // backlink, so a no-op call would quietly drop it.
    if ((storedProject.value || null) !== nextProject) {
      if (isNote.value) await notes.setProject(id, nextProject)
      else await todos.setProject(id, nextProject)
    }
    emit('saved')
  } finally {
    saving.value = false
  }
}

// ⌘/Ctrl+Enter saves. Plain Enter stays a newline (markdown is the common case).
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) {
    e.preventDefault()
    save()
  } else if (e.key === 'Escape' && props.escCancels) {
    e.preventDefault()
    emit('cancel')
  }
}

// Markdown links must not navigate the webview; hand them to the OS browser.
function onViewClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (/^(https?:|mailto:)/i.test(href)) openUrl(href).catch(() => { /* opener unavailable */ })
}

// A different item in the same host (next row in the drawer) starts clean.
watch(() => props.item?.id, reset, { immediate: true })
watch(
  () => props.editing,
  (on) => {
    if (on) focus()
  },
)

onMounted(() => {
  loadMarkdown()
  projects.fetchProjects()
  if (props.editing) focus()
})

defineExpose({ canSave, dirty, save, reset, draft, applyDraft, focus })
</script>

<template>
  <div v-if="item" @keydown="onKeydown">
    <!-- Edit mode -->
    <template v-if="editing">
      <Input
        v-if="isNote"
        ref="titleRef"
        v-model="title"
        class="mb-2 font-medium"
        :placeholder="t('item.titlePlaceholder')"
      />
      <Textarea
        ref="contentRef"
        v-model="content"
        class="min-h-[45vh] font-mono leading-relaxed"
        :placeholder="isNote ? t('item.notePlaceholder') : t('item.todoPlaceholder')"
      />
      <div class="mt-3 space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">
          {{ t('item.projectLabel') }}
        </label>
        <AppSelect
          v-model="projectId"
          :options="projectOptions"
          :placeholder="t('item.noProject')"
        >
          <template #leading>
            <FolderOpen class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
          </template>
        </AppSelect>
      </div>
      <p class="mt-2 text-[11px] text-muted-foreground">
        {{
          escCancels
            ? t('item.hint', { save: '⌘/Ctrl+Enter', esc: 'Esc' })
            : t('item.hintSaveOnly', { save: '⌘/Ctrl+Enter' })
        }}
      </p>
    </template>

    <!-- View mode -->
    <template v-else>
      <div
        v-if="storedContent.trim()"
        class="markdown-output text-sm"
        :class="done ? 'opacity-60 line-through decoration-muted-foreground/60' : ''"
        @click="onViewClick"
        v-html="renderText(storedContent)"
      />
      <p v-else class="text-sm text-muted-foreground italic">{{ t('item.empty') }}</p>

      <!-- Capture context: where this was jotted down -->
      <div
        v-if="projectName"
        class="mt-4 flex items-center gap-3 border-t border-border/60 pt-3 text-xs text-muted-foreground"
      >
        <span class="flex min-w-0 items-center gap-1.5">
          <FolderOpen class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="truncate">{{ projectName }}</span>
        </span>
        <button
          v-if="showRunLink && item.run_id"
          type="button"
          class="flex items-center gap-1.5 rounded px-1.5 py-1 text-primary transition-colors hover:bg-accent/60 cursor-pointer"
          :title="isNote ? t('item.openRunNoteTitle') : t('item.openRunTodoTitle')"
          @click="emit('openRun')"
        >
          <MessageSquare class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('item.openRun') }}
        </button>
      </div>
    </template>
  </div>
</template>
