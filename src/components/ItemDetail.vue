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
//
// The read view follows the capture window's layout (views/CaptureWindow.vue):
// a note's title as a large heading — renamed in place (click or F2 → inline
// field) —, one compact metadata line (age; full dates in its tooltip), the
// session backlink when offered, a divider, then the body as markdown. The
// project picker of the read view is the host's (the footer's left side); the
// edit form keeps its own project field, saved with the rest of the draft.
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { openFileWindow } from '@/lib/fileWindow'
import { MessageSquare } from 'lucide-vue-next'
import { Badge, Button, Input, Textarea } from '@/components/ui'
import InlineRenameInput from '@/components/InlineRenameInput.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { firstLine, relativeAge } from '@/lib/itemText'
import { useMarkdown } from '@/lib/markdown'
import { useNotesStore, type Note } from '@/stores/notes'
import { useTodosStore, type Todo } from '@/stores/todos'
import { useProjectsStore } from '@/stores/projects'
import { useToast } from '@/composables/useToast'

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
  /** Renamed / moved to another project straight from the read view. */
  changed: []
  /** Esc in edit mode (only when `escCancels`). */
  cancel: []
  openRun: []
}>()

const { t } = useI18n()
const notes = useNotesStore()
const todos = useTodosStore()
const projects = useProjectsStore()
const { toast } = useToast()
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

// ── Read view header ────────────────────────────────────────────────────────

/** A note's heading: its title, else its first line (as the list shows it). */
const heading = computed(
  () => storedTitle.value.trim() || firstLine(storedContent.value) || t('item.untitled'),
)

/** Compact age: a note by its last edit, a todo by its creation. */
const metaLine = computed(() => {
  const item = props.item
  if (!item) return ''
  return relativeAge(isNote.value ? (item as Note).updated_at : item.created_at)
})

/** Full dates for the metadata line's tooltip. */
const metaTooltip = computed(() => {
  const item = props.item
  if (!item) return ''
  const lines = [t('item.createdAt', { date: formatDate(item.created_at) })]
  const updated = isNote.value ? (item as Note).updated_at : null
  if (updated && updated !== item.created_at) {
    lines.push(t('item.updatedAt', { date: formatDate(updated) }))
  }
  return lines.join('\n')
})

function formatDate(iso: string): string {
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString()
}

const renaming = ref(false)

/** Inline rename of a note's title (the heading); todos have no title. */
function startRename() {
  if (!isNote.value || !props.item || props.editing) return
  renaming.value = true
}

/** Empty or unchanged → just close the field, no request. */
async function commitRename(value: string) {
  renaming.value = false
  if (!props.item || !value || value === storedTitle.value) return
  await notes.update(props.item.id, value, storedContent.value)
  if (!props.editing) title.value = storedTitle.value
  emit('changed')
}

// The host's project picker writes straight to the store while reading: keep
// the draft in step, or the next Edit would open already "dirty".
watch(storedProject, (value) => {
  if (!props.editing) projectId.value = value
})

// Absolute root of the note's project on disk, used to resolve relative file
// links (e.g. `[CV](sora/recruitment/cv/x.pdf)`) so they can be opened.
const projectPath = computed(
  () => projects.projects.find((p) => p.id === props.item?.project_id)?.path ?? null,
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

// Markdown links must not navigate the webview. External URLs go to the OS
// browser; a relative link is a file in the note's project — resolve it against
// the project root and open it in the app's own FileViewer window (so CV PDFs,
// images, etc. render inside the app instead of handing off to the OS browser).
async function onViewClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (!href) return
  if (/^(https?:|mailto:)/i.test(href)) {
    openUrl(href).catch(() => { /* opener unavailable */ })
    return
  }
  // Relative / absolute file path: strip any #fragment, decode %-escapes, then
  // resolve against the project root unless it is already an absolute path.
  let rel = href.replace(/#.*$/, '')
  try { rel = decodeURIComponent(rel) } catch { /* keep raw on malformed escapes */ }
  if (!rel) return
  // This window is its own webview with its own store copy; the projects list may
  // not have loaded yet when the click lands, so fetch it on demand.
  let root = projectPath.value
  if (!root) {
    await projects.fetchProjects()
    root = projectPath.value
  }
  if (!root) {
    toast.error(`Note chưa gắn project có đường dẫn (project_id=${props.item?.project_id ?? 'none'})`)
    return
  }
  const abs = rel.startsWith('/') ? rel : `${root.replace(/\/$/, '')}/${rel}`
  try {
    await openFileWindow(root, abs)
  } catch (err) {
    toast.error(`Không mở được: ${abs} — ${String(err)}`)
  }
}

// A different item in the same host (next row in the drawer) starts clean.
watch(() => props.item?.id, reset, { immediate: true })
watch(
  () => props.editing,
  (on) => {
    if (on) {
      renaming.value = false
      focus()
    }
  },
)

onMounted(() => {
  loadMarkdown()
  projects.fetchProjects()
  if (props.editing) focus()
})

defineExpose({ canSave, dirty, save, reset, draft, applyDraft, focus, startRename })
</script>

<template>
  <div v-if="item" class="flex min-h-0 flex-1 flex-col" @keydown="onKeydown">
    <!-- Edit mode -->
    <template v-if="editing">
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
        class="min-h-[8rem] flex-1 font-mono leading-relaxed"
        :placeholder="isNote ? t('item.notePlaceholder') : t('item.todoPlaceholder')"
      />
      <div class="mt-3 shrink-0 space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">
          {{ t('item.projectLabel') }}
        </label>
        <ProjectSelect
          variant="default"
          :model-value="projectId || null"
          @update:model-value="projectId = $event ?? NO_PROJECT"
        />
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
      <!-- Note title: the in-place rename (click or F2). -->
      <div v-if="isNote" class="flex shrink-0 items-center gap-1.5">
        <InlineRenameInput
          v-if="renaming"
          class="w-full text-lg font-semibold leading-snug"
          :value="storedTitle"
          :placeholder="t('item.titlePlaceholder')"
          @commit="commitRename"
          @cancel="renaming = false"
        />
        <h1
          v-else
          class="min-w-0 cursor-text rounded px-0.5 -mx-0.5 text-lg font-semibold leading-snug break-words transition-colors hover:bg-accent/50"
          :title="t('item.renameHint')"
          @click="startRename"
        >
          {{ heading }}
        </h1>
      </div>

      <!-- Compact metadata: age (full dates in the tooltip); a done todo says so. -->
      <div class="flex shrink-0 items-center gap-1.5" :class="isNote ? 'mt-1' : ''">
        <Badge v-if="done" tone="success" size="xs" class="shrink-0">{{ t('item.statusDone') }}</Badge>
        <p class="min-w-0 truncate text-xs text-muted-foreground" :title="metaTooltip">
          {{ metaLine }}
        </p>
      </div>

      <!-- The AI session it was captured from (the project picker is the host's). -->
      <div v-if="showRunLink && item.run_id" class="mt-2 flex shrink-0 flex-wrap items-center gap-1.5">
        <Button
          variant="outline"
          size="xs"
          :title="isNote ? t('item.openRunNoteTitle') : t('item.openRunTodoTitle')"
          @click="emit('openRun')"
        >
          <MessageSquare class="h-3 w-3" :stroke-width="1.75" />
          {{ t('item.openRun') }}
        </Button>
      </div>

      <div class="my-3 h-px shrink-0 bg-border/60" />

      <div
        v-if="storedContent.trim()"
        class="markdown-output text-sm"
        :class="done ? 'opacity-60 line-through decoration-muted-foreground/60' : ''"
        @click="onViewClick"
        v-html="renderText(storedContent)"
      />
      <p v-else class="text-sm text-muted-foreground italic">{{ t('item.empty') }}</p>
    </template>
  </div>
</template>
