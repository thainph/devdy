<script setup lang="ts">
// Detail drawer of one todo (spec §3.2): every field editable — title, project,
// status, deadline (date + time), priority, estimate, snooze, reminder, actual
// time — plus the source link, linked sessions and the AI section (FocusAiPanel:
// analyze, start with AI, the editable plan). Save sends only what changed
// (`update_todo_fields`); an estimate applied from the AI is saved as `ai`.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, ExternalLink, Link as LinkIcon, Trash2, Undo2, X } from 'lucide-vue-next'
import { AppSelect, Button, Drawer, Input, Textarea } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import {
  emptyTodoDraft,
  formatEstimate,
  fromLocalInput,
  parseEstimate,
  rebaseForm,
  sourceShortLabel,
  toLocalInput,
  type AiApply,
  type FocusItem,
  type FocusTodo,
  type TodoDraft,
  type TodoPatch,
  type TodoStatus,
} from '@/lib/focus'
import TodoMetaFields from './TodoMetaFields.vue'
import FocusAiPanel from './FocusAiPanel.vue'
import { SOURCE_ICON } from './sourceIcon'
import { useFocusActions } from './useFocusActions'
import { useFocusText } from './useFocusText'

const props = defineProps<{ item: FocusItem | null }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const store = useFocusStore()
const actions = useFocusActions()
const { stamp } = useFocusText()

const draft = ref<TodoDraft>(emptyTodoDraft())
const status = ref<TodoStatus>('todo')
const snoozedUntil = ref('')
const remindAt = ref('')
const actual = ref('')
const saving = ref(false)
/** Minutes last applied from an AI analysis: saved with `estimate_source = 'ai'` while unchanged. */
const aiEstimate = ref<number | null>(null)

const todo = computed(() => props.item?.todo ?? null)

/** Every editable field of the form, flat (for `rebaseForm`). */
interface DrawerForm extends TodoDraft {
  [key: string]: unknown
  status: TodoStatus
  snoozedUntil: string
  remindAt: string
  actual: string
}

function formOf(td: FocusTodo): DrawerForm {
  return {
    ...emptyTodoDraft({
      text: td.text,
      projectId: td.project_id ?? '',
      dueDate: td.due_date ?? '',
      dueTime: td.due_time ?? '',
      priority: td.priority,
      estimate: formatEstimate(td.estimate_min),
    }),
    status: td.status,
    snoozedUntil: toLocalInput(td.snoozed_until),
    remindAt: toLocalInput(td.remind_at),
    actual: formatEstimate(td.actual_min),
  }
}

function currentForm(): DrawerForm {
  return {
    ...draft.value,
    status: status.value,
    snoozedUntil: snoozedUntil.value,
    remindAt: remindAt.value,
    actual: actual.value,
  }
}

function setForm(f: DrawerForm) {
  const { status: st, snoozedUntil: sn, remindAt: rm, actual: ac, ...d } = f
  draft.value = d as TodoDraft
  status.value = st
  snoozedUntil.value = sn
  remindAt.value = rm
  actual.value = ac
}

/** The form as last loaded from the todo (what "untouched" means). */
let base: DrawerForm | null = null

function reset() {
  const td = todo.value
  if (!td) return
  base = formOf(td)
  setForm(base)
  aiEstimate.value = null
}

/** Same todo refreshed (e.g. status → doing after Start with AI): untouched fields follow it. */
function rebase() {
  const td = todo.value
  if (!td || !base) return
  const next = formOf(td)
  setForm(rebaseForm(currentForm(), base, next))
  base = next
}

/** "Apply" in the AI panel fills the form; the drawer's Save stores it (BR-09). */
function onAiApply(a: AiApply) {
  const d = { ...draft.value }
  if (a.estimateMin != null) {
    d.estimate = formatEstimate(a.estimateMin)
    aiEstimate.value = a.estimateMin
  }
  if (a.priority) d.priority = a.priority
  if (a.dueDate) {
    d.dueDate = a.dueDate
    d.dueTime = a.dueTime ?? ''
  }
  draft.value = d
}

// A different todo → load it; the same one refreshed by the board keeps the
// user's edits and takes every other change.
watch(() => todo.value?.id, reset, { immediate: true })
watch(todo, (td, prev) => {
  if (td && prev && td.id === prev.id) rebase()
})

const projectId = computed({
  get: () => draft.value.projectId || null,
  set: (v: string | null) => (draft.value = { ...draft.value, projectId: v ?? '' }),
})

const STATUSES: TodoStatus[] = ['todo', 'doing', 'waiting']
const statusOptions = computed(() => STATUSES.map((s) => ({ value: s, label: t(`focus.status.${s}`) })))

const actualMin = computed(() => parseEstimate(actual.value))

const patch = computed<TodoPatch | null>(() => {
  const td = todo.value
  if (!td) return null
  const d = draft.value
  const estimate = parseEstimate(d.estimate)
  if (!d.text.trim() || estimate === undefined || actualMin.value === undefined) return null
  const p: TodoPatch = {}
  if (d.text.trim() !== td.text) p.text = d.text.trim()
  if ((d.projectId || null) !== td.project_id) p.project_id = d.projectId || null
  if ((d.dueDate || null) !== td.due_date) p.due_date = d.dueDate || null
  // Clearing the date clears the time on the backend; only send a time with a date.
  const dueTime = d.dueDate && d.dueTime ? d.dueTime : null
  if (d.dueDate && dueTime !== td.due_time) p.due_time = dueTime
  if (d.priority !== td.priority) p.priority = d.priority
  // The backend marks a set estimate `estimate_source = 'user'` itself, unless told it came from the AI.
  if (estimate !== td.estimate_min) {
    p.estimate_min = estimate
    if (estimate != null && estimate === aiEstimate.value) p.estimate_source = 'ai'
  }
  if (status.value !== td.status) p.status = status.value
  const snooze = fromLocalInput(snoozedUntil.value)
  if (snooze !== (td.snoozed_until ? fromLocalInput(toLocalInput(td.snoozed_until)) : null)) p.snoozed_until = snooze
  const remind = fromLocalInput(remindAt.value)
  if (remind !== (td.remind_at ? fromLocalInput(toLocalInput(td.remind_at)) : null)) p.remind_at = remind
  if (actualMin.value !== td.actual_min) p.actual_min = actualMin.value
  return p
})
const dirty = computed(() => !!patch.value && Object.keys(patch.value).length > 0)
const invalid = computed(() => !!todo.value && patch.value === null)

async function save() {
  const td = todo.value
  if (!td || !patch.value || !dirty.value || saving.value) return
  saving.value = true
  try {
    await store.update(td.id, patch.value)
    toast.success(t('focus.toast.saved'))
    emit('close')
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

async function close() {
  if (dirty.value) {
    const ok = await confirm({
      message: t('focus.drawer.unsaved'),
      confirmLabel: t('focus.drawer.discard'),
      cancelLabel: t('common.cancel'),
    })
    if (!ok) return
  }
  emit('close')
}

async function onDelete() {
  if (todo.value && (await actions.remove(todo.value))) emit('close')
}

async function toggleDone() {
  const td = todo.value
  if (!td) return
  if (td.done) await actions.undone(td)
  else {
    await actions.done(td)
    emit('close')
  }
}

const sourceText = computed(() => {
  const td = todo.value
  if (!td?.source_url) return ''
  return sourceShortLabel(td.source_type, td.source_ref, td.source_url) ?? td.source_ref ?? td.source_url
})

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) {
    e.preventDefault()
    void save()
  }
}
</script>

<template>
  <Drawer :open="!!item" size="lg" :dismiss-on-overlay="false" @close="close">
    <template #header>
      <h3 class="min-w-0 flex-1 truncate text-sm font-semibold">{{ todo?.text.split('\n')[0] }}</h3>
    </template>

    <div v-if="item && todo" class="space-y-4 p-4 text-sm" @keydown="onKeydown">
      <div class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.drawer.text') }}</label>
        <Textarea v-model="draft.text" class="min-h-[4.5rem] w-full" />
      </div>

      <div class="grid grid-cols-2 gap-2.5">
        <div class="space-y-1">
          <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.drawer.project') }}</label>
          <ProjectSelect v-model="projectId" variant="default" size="sm" />
        </div>
        <div class="space-y-1">
          <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.status.label') }}</label>
          <AppSelect v-model="status" :options="statusOptions" :disabled="todo.done" />
        </div>
      </div>

      <TodoMetaFields v-model="draft" />
      <p v-if="item.effective_estimate_min !== todo.estimate_min" class="-mt-2 text-[11px] text-muted-foreground">
        {{ t('focus.drawer.effectiveEstimate', { estimate: formatEstimate(item.effective_estimate_min) }) }}
        <template v-if="todo.estimate_source === 'ai'"> · {{ t('focus.estimate.ai') }}</template>
      </p>

      <div class="grid grid-cols-2 gap-2.5">
        <div class="space-y-1">
          <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.drawer.snoozedUntil') }}</label>
          <div class="flex items-center gap-1">
            <Input v-model="snoozedUntil" type="datetime-local" size="sm" class="min-w-0 flex-1" />
            <Button v-if="snoozedUntil" variant="ghost" size="icon-sm" :title="t('focus.drawer.clear')" :aria-label="t('focus.drawer.clear')" @click="snoozedUntil = ''">
              <X class="h-3 w-3" :stroke-width="2" />
            </Button>
          </div>
        </div>
        <div class="space-y-1">
          <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.drawer.remindAt') }}</label>
          <div class="flex items-center gap-1">
            <Input v-model="remindAt" type="datetime-local" size="sm" class="min-w-0 flex-1" />
            <Button v-if="remindAt" variant="ghost" size="icon-sm" :title="t('focus.drawer.clear')" :aria-label="t('focus.drawer.clear')" @click="remindAt = ''">
              <X class="h-3 w-3" :stroke-width="2" />
            </Button>
          </div>
        </div>
      </div>

      <div v-if="todo.done || todo.started_at || todo.actual_min != null" class="space-y-1">
        <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.drawer.actual') }}</label>
        <Input
          v-model="actual"
          size="sm"
          class="w-40"
          :class="actualMin === undefined ? 'border-destructive' : ''"
          :placeholder="t('focus.drawer.estimatePlaceholder')"
        />
        <p v-if="actualMin === undefined" class="text-[11px] text-destructive">{{ t('focus.drawer.estimateInvalid') }}</p>
      </div>

      <!-- Source + linked sessions -->
      <div v-if="todo.source_url || todo.linked_run_count > 0" class="flex flex-wrap items-center gap-3 text-xs">
        <button
          v-if="todo.source_url"
          type="button"
          class="inline-flex min-w-0 cursor-pointer items-center gap-1.5 rounded-md border border-border px-2 py-1 transition-colors hover:bg-accent"
          :title="todo.source_url"
          @click="actions.openSource(todo)"
        >
          <component :is="SOURCE_ICON[todo.source_type ?? 'url']" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="max-w-72 truncate">{{ sourceText }}</span>
          <ExternalLink class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        </button>
        <span v-if="todo.linked_run_count > 0" class="inline-flex items-center gap-1 text-muted-foreground">
          <LinkIcon class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('focus.actions.sessions', { n: todo.linked_run_count }, todo.linked_run_count) }}
        </span>
      </div>

      <FocusAiPanel :todo="todo" @apply="onAiApply" />

      <div class="space-y-0.5 text-[11px] text-muted-foreground">
        <p>{{ t('focus.drawer.created', { time: stamp(todo.created_at) }) }}</p>
        <p v-if="todo.started_at">{{ t('focus.drawer.started', { time: stamp(todo.started_at) }) }}</p>
        <p v-if="todo.completed_at">{{ t('focus.drawer.completed', { time: stamp(todo.completed_at) }) }}</p>
      </div>
    </div>

    <template #footer>
      <Button v-if="todo" variant="outline" size="sm" @click="toggleDone">
        <component :is="todo.done ? Undo2 : Check" class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ todo.done ? t('focus.actions.undone') : t('focus.actions.done') }}
      </Button>
      <Button variant="destructive-ghost" size="icon-sm" :title="t('focus.actions.delete')" :aria-label="t('focus.actions.delete')" @click="onDelete">
        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <span class="flex-1" />
      <Button variant="ghost" size="sm" @click="close">{{ t('focus.drawer.cancel') }}</Button>
      <Button size="sm" :disabled="!dirty || invalid || saving" @click="save">{{ t('focus.drawer.save') }}</Button>
    </template>
  </Drawer>
</template>
