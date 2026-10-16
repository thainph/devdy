<script setup lang="ts">
// Body of the Notes / Todos window's detail pane: the item as it reads (its
// markdown) or, while `editing`, its inline editor — a note's title + body, a
// todo's text — plus the save itself. The project lives in the header.
//
// ⌘/Ctrl+Enter saves, Esc cancels (both only inside the fields). The host
// drives it through `editing` and the exposed `save()` / `reset()` / `focus()`.
//
// Markdown links never navigate the webview: URLs go to the OS browser; a
// relative link is a file in the item's project, opened in the app's own file
// viewer window.
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Input, Textarea } from '@/components/ui'
import { openFileWindow } from '@/lib/fileWindow'
import { useMarkdown } from '@/lib/markdown'
import { useToast } from '@/composables/useToast'
import { useNotesStore, type Note } from '@/stores/notes'
import { useTodosStore, type Todo } from '@/stores/todos'
import { useProjectsStore } from '@/stores/projects'

const props = defineProps<{
  kind: 'note' | 'todo'
  item: Note | Todo
  /** false renders the item as it reads; true renders the fields. */
  editing: boolean
}>()

const emit = defineEmits<{
  /** Written to the DB; the host leaves editing and broadcasts. */
  saved: []
  /** Esc inside the fields. */
  cancel: []
}>()

const { t } = useI18n()
const { toast } = useToast()
const notes = useNotesStore()
const todos = useTodosStore()
const projects = useProjectsStore()
const { renderText, loadMarkdown } = useMarkdown()

const isNote = computed(() => props.kind === 'note')

// What is stored — the yardstick for `dirty` and `reset`.
const storedTitle = computed(() => (isNote.value ? (props.item as Note).title : ''))
const storedContent = computed(() =>
  isNote.value ? (props.item as Note).content : (props.item as Todo).text,
)
const done = computed(() => !isNote.value && (props.item as Todo).done)

// Draft fields: `title` is note-only; `content` is the note body / todo text.
const title = ref('')
const content = ref('')
const saving = ref(false)
const titleRef = ref<InstanceType<typeof Input> | null>(null)
const contentRef = ref<InstanceType<typeof Textarea> | null>(null)

const dirty = computed(
  () => title.value !== storedTitle.value || content.value !== storedContent.value,
)

// A note may lose its title or its body, but not both; a todo must keep text —
// saving it empty would delete the row (see the todos store), and Delete is the
// only thing allowed to do that.
const canSave = computed(() =>
  isNote.value ? !!(title.value.trim() || content.value.trim()) : content.value.trim().length > 0,
)

/** Reload the fields from what is stored, dropping unsaved edits. */
function reset() {
  title.value = storedTitle.value
  content.value = storedContent.value
}

function focus() {
  nextTick(() => {
    // With a title already there, the body is where the user wants to land.
    const el = isNote.value && !title.value ? titleRef.value?.$el : contentRef.value?.$el
    ;(el as HTMLElement | undefined)?.focus()
  })
}

async function save() {
  if (!canSave.value || saving.value) return
  if (!dirty.value) {
    emit('saved')
    return
  }
  saving.value = true
  // The stores swallow write errors (rolled back + `error` set): clear it first
  // so a failed save keeps the fields open instead of dropping the draft.
  const store = isNote.value ? notes : todos
  store.error = null
  try {
    if (isNote.value) await notes.update(props.item.id, title.value, content.value)
    else await todos.update(props.item.id, content.value)
    if (store.error) {
      toast.error(t('item.saveFailed', { error: store.error }))
      return
    }
    emit('saved')
  } finally {
    saving.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    void save()
  } else if (e.key === 'Escape') {
    e.preventDefault()
    emit('cancel')
  }
}

const projectPath = computed(
  () => projects.projects.find((p) => p.id === props.item.project_id)?.path ?? null,
)

async function onViewClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (!href) return
  if (/^(https?:|mailto:)/i.test(href)) {
    openUrl(href).catch(() => {
      /* opener unavailable */
    })
    return
  }
  // A file path: strip any #fragment, decode %-escapes, then resolve against the
  // project root unless it is already absolute.
  let rel = href.replace(/#.*$/, '')
  try {
    rel = decodeURIComponent(rel)
  } catch {
    /* keep raw on malformed escapes */
  }
  if (!rel) return
  let root = projectPath.value
  if (!root) {
    await projects.fetchProjects()
    root = projectPath.value
  }
  if (!root) {
    toast.error(t('item.noProjectPath'))
    return
  }
  const abs = rel.startsWith('/') ? rel : `${root.replace(/\/$/, '')}/${rel}`
  try {
    await openFileWindow(root, abs)
  } catch (err) {
    toast.error(t('item.openFileFailed', { path: abs, error: String(err) }))
  }
}

// Another item starts clean; entering the fields starts from what is stored.
watch(() => props.item.id, reset, { immediate: true })
watch(
  () => props.editing,
  (on) => {
    if (on) {
      reset()
      focus()
    }
  },
)

onMounted(loadMarkdown)

/** The unsaved fields as markdown (a note's title as its heading). */
function draftText(): string {
  return title.value.trim() ? `# ${title.value.trim()}\n\n${content.value}` : content.value
}

defineExpose({ canSave, dirty, saving, save, reset, focus, draftText })
</script>

<template>
  <!-- Editing: the fields fill the pane. -->
  <div v-if="editing" class="flex min-h-0 flex-1 flex-col px-5 py-4" @keydown="onKeydown">
    <Input
      v-if="isNote"
      ref="titleRef"
      v-model="title"
      class="mb-2 shrink-0 font-medium"
      :placeholder="t('item.titlePlaceholder')"
    />
    <Textarea
      ref="contentRef"
      v-model="content"
      class="min-h-[8rem] flex-1 resize-none font-mono leading-relaxed"
      :placeholder="isNote ? t('item.notePlaceholder') : t('item.todoPlaceholder')"
    />
    <p class="mt-2 shrink-0 text-[11px] text-muted-foreground">
      {{ t('item.hint', { save: '⌘/Ctrl+Enter', esc: 'Esc' }) }}
    </p>
  </div>

  <!-- Reading: the markdown. -->
  <div v-else class="min-h-0 flex-1 overflow-y-auto px-5 py-4">
    <div
      v-if="storedContent.trim()"
      class="markdown-output text-sm"
      :class="done ? 'opacity-60 line-through decoration-muted-foreground/60' : ''"
      @click="onViewClick"
      v-html="renderText(storedContent)"
    />
    <p v-else class="text-sm italic text-muted-foreground">{{ t('item.empty') }}</p>
  </div>
</template>
