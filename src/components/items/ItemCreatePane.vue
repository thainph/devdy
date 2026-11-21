<script setup lang="ts">
// The new todo / note form of the Todos / Notes screen's detail pane, laid out
// like an item's detail (ItemHeader's two tiers) so a saved item takes its
// place without the layout jumping:
//
//   ┌ [☰] What needs doing?                                (borderless, ≤3 lines)
//   │ New todo · [💬 From session: … ×]   [project ▾] [📅 Due ×] | Cancel  [Add todo]
//   ├ todo: Priority / Estimate / Time / Source (TodoPropertyList)
//   │ note: the markdown body, full height, borderless
//   └ [◉ Keep adding after save]   ⌘↵ save · Esc cancel
//
// The draft lives in useQuickCapture (survives switching tabs / items); saving
// validates through lib/focus draftToInput (estimate parsing). Saved: `saved`
// with the new id — the host opens it, or, with "Keep adding" on, the form
// clears for the next one (the switch is remembered).
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { MessageSquare, X } from 'lucide-vue-next'
import { Button, DatePicker, ToggleSwitch } from '@/components/ui'
import ListPaneButtons from '@/components/listDetail/ListPaneButtons.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import TodoPropertyList from './TodoPropertyList.vue'
import { useToast } from '@/composables/useToast'
import { useTodosStore } from '@/stores/todos'
import { useNotesStore } from '@/stores/notes'
import { useProjectsStore } from '@/stores/projects'
import type { RunRecord } from '@/stores/runs'
import { useQuickCapture } from '@/composables/useQuickCapture'
import { useSessionText } from '@/composables/useSessionText'
import { draftToInput } from '@/lib/focus'
import { invoke } from '@/lib/tauri'
import { IS_MAC } from '@/lib/appMenu'

const props = defineProps<{ kind: 'note' | 'todo' }>()

const emit = defineEmits<{
  /** `id`: the created item (null if the store couldn't return it); `keepGoing`: the form stays. */
  saved: [id: string | null, keepGoing: boolean]
  /** Cancel / Esc. */
  dismiss: []
}>()

const { t } = useI18n()
const { toast } = useToast()
const todos = useTodosStore()
const notes = useNotesStore()
const projects = useProjectsStore()
const { runLabel } = useSessionText()
const { todoText, todoMeta, noteTitle, noteContent, projectId, contextProjectId, runId, clearDraft } = useQuickCapture()

const isTodo = computed(() => props.kind === 'todo')

/** The big title: the todo's text, or the note's title. */
const title = computed({
  get: () => (isTodo.value ? todoText.value : noteTitle.value),
  set: (v: string) => {
    if (isTodo.value) todoText.value = v
    else noteTitle.value = v
  },
})

// The run backlink only survives while the capture stays on the project it came
// from — re-pointing it at another project would make the link nonsense.
const effectiveRunId = computed(() =>
  runId.value && projectId.value && projectId.value === contextProjectId.value ? runId.value : null,
)

/** The name of the session the capture came from (looked up in its project's runs). */
const runName = ref<string | null>(null)
watch(
  [runId, contextProjectId],
  async ([id, project]) => {
    runName.value = null
    if (!id || !project) return
    try {
      const run = (await invoke<RunRecord[]>('list_runs', { projectId: project })).find((r) => r.id === id)
      if (run && runId.value === id) {
        runName.value = runLabel({ title: run.title, runType: run.run_type, refNumber: run.ref_number })
      }
    } catch {
      /* the chip falls back to "Untitled session" */
    }
  },
  { immediate: true },
)

// The todo's add_todo args (null while the text is empty or the estimate can't be read).
const todoInput = computed(() => draftToInput({ ...todoMeta.value, text: todoText.value }))

const canSubmit = computed(() =>
  isTodo.value ? !!todoInput.value : noteTitle.value.trim().length > 0 || noteContent.value.trim().length > 0,
)

const KEEP_KEY = 'items.keepAdding'
function loadKeep(): boolean {
  try {
    return localStorage.getItem(KEEP_KEY) === '1'
  } catch {
    return false
  }
}
const keepAdding = ref(loadKeep())
function toggleKeep() {
  keepAdding.value = !keepAdding.value
  try {
    localStorage.setItem(KEEP_KEY, keepAdding.value ? '1' : '0')
  } catch {
    /* storage unavailable */
  }
}

// ── Title: auto-grows up to 3 lines ────────────────────────────────────────

const titleRef = ref<HTMLTextAreaElement | null>(null)
const bodyRef = ref<HTMLTextAreaElement | null>(null)

function grow() {
  const el = titleRef.value
  if (!el) return
  el.style.height = 'auto'
  const line = parseFloat(getComputedStyle(el).lineHeight) || 20
  el.style.height = `${Math.min(el.scrollHeight, line * 3 + 4)}px`
}
watch(title, () => nextTick(grow))

/** Land in the title — a note with its title already filled in lands in the body. */
function focus() {
  nextTick(() => {
    grow()
    const el = !isTodo.value && noteTitle.value ? bodyRef.value : titleRef.value
    el?.focus()
    if (el) el.setSelectionRange(el.value.length, el.value.length)
  })
}

function setDue(date: string) {
  todoMeta.value = { ...todoMeta.value, dueDate: date, dueTime: date ? todoMeta.value.dueTime : '' }
}

// ── Save ────────────────────────────────────────────────────────────────────

const saving = ref(false)

async function submit() {
  if (!canSubmit.value || saving.value) return
  saving.value = true
  const project = projectId.value || null
  const keep = keepAdding.value
  try {
    let id: string | null = null
    if (isTodo.value) {
      const { dueDate, dueTime, priority, estimateMin, sourceUrl } = todoInput.value!
      const todo = await todos.add(todoText.value, project, effectiveRunId.value, { dueDate, dueTime, priority, estimateMin, sourceUrl })
      id = todo?.id ?? null
      toast.success(t('item.todoAdded'))
    } else {
      const note = await notes.add(noteTitle.value, noteContent.value, project, effectiveRunId.value)
      id = note?.id ?? null
      toast.success(t('item.noteSaved'))
    }
    clearDraft(props.kind)
    emit('saved', id, keep)
    if (keep) focus()
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

// ⌘/Ctrl+Enter saves; Esc cancels. In a note's title, Enter moves to the body
// (a todo's text may span lines).
function onKeydown(e: KeyboardEvent) {
  if (e.isComposing) return
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    void submit()
  } else if (e.key === 'Escape' && !e.defaultPrevented) {
    e.preventDefault()
    emit('dismiss')
  }
}
function onTitleKeydown(e: KeyboardEvent) {
  if (!isTodo.value && e.key === 'Enter' && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.isComposing) {
    e.preventDefault()
    bodyRef.value?.focus()
  }
}

const mod = IS_MAC ? '⌘' : 'Ctrl+'
const kbdClass = 'rounded border border-border/70 bg-muted/60 px-1 font-mono text-[10px] text-muted-foreground'

onMounted(() => {
  void projects.fetchProjects()
  focus()
})

defineExpose({ submit, canSubmit, saving, focus })
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col" @keydown="onKeydown">
    <!-- Header: the same two tiers as ItemHeader. -->
    <div class="shrink-0 border-b border-border/60 px-5 pb-2 pt-2.5">
      <div class="flex items-start gap-3">
        <ListPaneButtons mode="show" />
        <textarea
          ref="titleRef"
          v-model="title"
          rows="1"
          class="-mx-1 min-w-0 flex-1 resize-none overflow-y-auto rounded bg-transparent px-1 py-0.5 text-[15px] font-bold leading-snug text-foreground outline-none transition-colors placeholder:font-semibold placeholder:text-muted-foreground/60 focus:bg-accent/40"
          :placeholder="isTodo ? t('item.create.titleTodo') : t('item.create.titleNote')"
          :aria-label="isTodo ? t('item.create.titleTodo') : t('item.create.titleNote')"
          @keydown="onTitleKeydown"
        />
      </div>
      <div class="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <p class="flex min-w-0 flex-1 basis-40 items-center gap-1.5 text-xs text-muted-foreground">
          <span class="shrink-0">{{ isTodo ? t('item.create.metaTodo') : t('item.create.metaNote') }}</span>
          <span
            v-if="effectiveRunId"
            class="inline-flex min-w-0 items-center gap-1 rounded-full bg-primary/12 py-px pl-2 pr-0.5 text-primary"
            :title="t('item.create.fromSessionHint')"
          >
            <MessageSquare class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="min-w-0 truncate">
              {{ t('item.create.fromSession', { name: runName ?? t('sessionLink.untitledSession') }) }}
            </span>
            <button
              type="button"
              class="flex h-4 w-4 shrink-0 cursor-pointer items-center justify-center rounded-full opacity-70 transition-opacity hover:bg-primary/15 hover:opacity-100"
              :title="t('item.create.dropRunLink')"
              :aria-label="t('item.create.dropRunLink')"
              @click="runId = null"
            >
              <X class="h-2.5 w-2.5" :stroke-width="2.25" />
            </button>
          </span>
        </p>
        <div class="ml-auto flex min-w-0 flex-wrap items-center justify-end gap-1.5">
          <div class="w-[11rem] min-w-[5.5rem] shrink">
            <ProjectSelect
              :model-value="projectId || null"
              variant="outline"
              size="sm"
              @update:model-value="projectId = $event ?? ''"
            />
          </div>
          <!-- A todo's deadline (× clears it, and its time). -->
          <div v-if="isTodo" class="flex w-[9.5rem] min-w-[7rem] shrink items-center">
            <DatePicker
              :model-value="todoMeta.dueDate"
              size="sm"
              class="min-w-0 flex-1"
              :class="todoMeta.dueDate ? 'rounded-r-none' : ''"
              :placeholder="t('item.create.due')"
              @update:model-value="setDue($event)"
            />
            <button
              v-if="todoMeta.dueDate"
              type="button"
              class="flex h-8 w-7 shrink-0 cursor-pointer items-center justify-center rounded-r-md border border-l-0 border-border bg-background text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              :title="t('item.clearDueDate')"
              :aria-label="t('item.clearDueDate')"
              @click="setDue('')"
            >
              <X class="h-3 w-3" :stroke-width="2" />
            </button>
          </div>
          <div class="mx-0.5 h-4 w-px shrink-0 bg-border" aria-hidden="true" />
          <Button variant="outline" size="sm" class="shrink-0" @click="emit('dismiss')">
            {{ t('common.cancel') }}
          </Button>
          <Button
            variant="primary"
            size="sm"
            class="shrink-0"
            :disabled="!canSubmit || saving"
            :title="`${mod}Enter`"
            @click="submit"
          >
            {{ isTodo ? t('item.addTodo') : t('item.saveNote') }}
          </Button>
        </div>
      </div>
    </div>

    <!-- Body -->
    <div class="flex min-h-0 flex-1 flex-col overflow-auto px-5 py-3.5">
      <TodoPropertyList v-if="isTodo" v-model="todoMeta" />
      <textarea
        v-else
        ref="bodyRef"
        v-model="noteContent"
        class="min-h-[12rem] w-full flex-1 resize-none bg-transparent text-sm leading-relaxed text-foreground outline-none placeholder:text-muted-foreground/60"
        :placeholder="t('item.create.body')"
        :aria-label="t('item.create.body')"
      />
    </div>

    <!-- Footer -->
    <div class="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-t border-border/60 px-5 py-2 text-[11px] text-muted-foreground">
      <div class="flex items-center gap-2">
        <ToggleSwitch :checked="keepAdding" size="sm" :label="t('item.create.keepAdding')" @toggle="toggleKeep" />
        <span class="cursor-pointer select-none" @click="toggleKeep">{{ t('item.create.keepAdding') }}</span>
      </div>
      <span class="flex items-center gap-1">
        <kbd :class="kbdClass">{{ mod }}↵</kbd> {{ t('item.create.save') }} ·
        <kbd :class="kbdClass">Esc</kbd> {{ t('item.create.cancel') }}
      </span>
    </div>
  </section>
</template>
