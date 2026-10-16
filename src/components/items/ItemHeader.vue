<script setup lang="ts">
// Header of the Notes / Todos window's detail pane — the sibling of
// SlackThreadHeader, same layout and controls:
//
//   note  title (click / F2 → inline rename) / updated · N characters
//   todo  ☐ text's first line (click / F2 → edit) / created · from <session>
//   right [Project ▾][Sessions (n) ▾] | (todo) ✓ Done · ✎ Edit · (note) ⧉ Copy
//         markdown · ⋯ (New session from this … · Copy ID · Delete)
//
// While editing, the actions give way to Cancel · Save (⌘/Ctrl+Enter / Esc do
// the same inside the fields). Narrow pane: the title gives way first, then the
// project name truncates.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Pencil } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import InlineRenameInput from '@/components/InlineRenameInput.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import CopyMarkdownButton from '@/components/CopyMarkdownButton.vue'
import LinkedSessionsMenu from '@/components/sessions/LinkedSessionsMenu.vue'
import DetailActionsMenu from '@/components/sessions/DetailActionsMenu.vue'
import { useSessionText } from '@/composables/useSessionText'
import { firstLine, relativeAge } from '@/lib/itemText'
import { formatFullDate } from '@/lib/slackThreadUtils'
import type { SessionLink } from '@/lib/sessionLinks'
import type { Note } from '@/stores/notes'
import type { Todo } from '@/stores/todos'

const props = defineProps<{
  kind: 'note' | 'todo'
  item: Note | Todo
  /** Sessions linked to the item. */
  links: SessionLink[]
  renaming: boolean
  editing: boolean
  canSave: boolean
  saving: boolean
  /** Copy ID just fired: its icon becomes a tick. */
  copiedId: boolean
}>()

const emit = defineEmits<{
  startRename: []
  commitRename: [title: string]
  cancelRename: []
  edit: []
  cancelEdit: []
  save: []
  toggleDone: []
  setProject: [projectId: string | null]
  openRun: [link: SessionLink]
  unlinkRun: [link: SessionLink]
  linkRun: [runId: string]
  newSession: []
  copyId: []
  delete: []
}>()

const { t } = useI18n()
const { runLabel } = useSessionText()

const isNote = computed(() => props.kind === 'note')
const note = computed(() => (isNote.value ? (props.item as Note) : null))
const todo = computed(() => (isNote.value ? null : (props.item as Todo)))
const body = computed(() => note.value?.content ?? todo.value?.text ?? '')

const heading = computed(
  () => note.value?.title.trim() || firstLine(body.value) || t('item.untitled'),
)

/** note: updated · N characters; todo: created · from <the session it was written in>. */
const meta = computed(() => {
  if (note.value) {
    const chars = [...note.value.content].length
    return [
      t('item.metaUpdated', { age: relativeAge(note.value.updated_at) }),
      t('item.metaChars', { n: chars }, chars),
    ].join(' · ')
  }
  const parts = [t('item.metaCreated', { age: relativeAge(props.item.created_at) })]
  const captured = props.links.find((l) => l.source === 'captured')
  if (captured) parts.push(t('item.metaFromSession', { name: runLabel(captured) }))
  return parts.join(' · ')
})

const metaTitle = computed(() => {
  const lines = [t('item.createdAt', { date: formatFullDate(props.item.created_at) })]
  if (note.value && note.value.updated_at !== note.value.created_at) {
    lines.push(t('item.updatedAt', { date: formatFullDate(note.value.updated_at) }))
  }
  return lines.join('\n')
})

const newSessionLabel = computed(() =>
  isNote.value ? t('item.newSessionNote') : t('item.newSessionTodo'),
)
</script>

<template>
  <div class="flex shrink-0 items-center gap-3 border-b border-border/60 px-5 py-2.5">
    <div class="min-w-[6rem] flex-1">
      <div class="flex min-w-0 items-center gap-1.5">
        <!-- Todo: its checkbox leads the line. -->
        <button
          v-if="todo"
          type="button"
          class="flex h-4 w-4 shrink-0 cursor-pointer items-center justify-center rounded-[5px] border transition-colors"
          :class="todo.done
            ? 'border-primary bg-primary text-primary-foreground'
            : 'border-muted-foreground/40 hover:border-primary'"
          :aria-pressed="todo.done"
          :title="todo.done ? t('item.markNotDone') : t('item.markDone')"
          :aria-label="todo.done ? t('item.markNotDone') : t('item.markDone')"
          @click="emit('toggleDone')"
        >
          <Check v-if="todo.done" class="h-2.5 w-2.5" :stroke-width="3" />
        </button>
        <InlineRenameInput
          v-if="renaming && note"
          class="w-full text-[15px] font-bold leading-snug"
          :value="note.title"
          :placeholder="t('item.titlePlaceholder')"
          @commit="emit('commitRename', $event)"
          @cancel="emit('cancelRename')"
        />
        <h1
          v-else
          class="min-w-0 cursor-text truncate rounded px-0.5 -mx-0.5 text-[15px] font-bold leading-snug transition-colors hover:bg-accent/50"
          :class="todo?.done && 'text-muted-foreground line-through decoration-muted-foreground/60'"
          :title="`${heading}\n${note ? t('item.renameHint') : t('item.editTitle')}`"
          @click="note ? emit('startRename') : emit('edit')"
        >
          {{ heading }}
        </h1>
      </div>
      <p class="mt-0.5 truncate text-xs text-muted-foreground" :title="metaTitle">{{ meta }}</p>
    </div>

    <div class="flex min-w-0 items-center gap-1.5">
      <!-- The item's project and its sessions: linking depends on the project. -->
      <div class="w-[11rem] min-w-[5.5rem] shrink">
        <ProjectSelect
          :model-value="item.project_id"
          variant="outline"
          size="xs"
          @update:model-value="emit('setProject', $event)"
        />
      </div>
      <LinkedSessionsMenu
        class="shrink-0"
        :project-id="item.project_id"
        :links="links"
        @open-run="emit('openRun', $event)"
        @unlink-run="emit('unlinkRun', $event)"
        @link="emit('linkRun', $event)"
      />
      <div class="mx-0.5 h-4 w-px shrink-0 bg-border" aria-hidden="true" />

      <template v-if="editing">
        <Button variant="outline" size="xs" class="shrink-0" @click="emit('cancelEdit')">
          {{ t('common.cancel') }}
        </Button>
        <Button
          variant="primary"
          size="xs"
          class="shrink-0"
          :disabled="!canSave || saving"
          :title="'⌘/Ctrl+Enter'"
          @click="emit('save')"
        >
          {{ t('common.save') }}
        </Button>
      </template>
      <template v-else>
        <Button
          v-if="todo"
          :variant="todo.done ? 'primary' : 'outline'"
          size="icon-sm"
          class="shrink-0"
          :aria-pressed="todo.done"
          :title="todo.done ? t('item.markNotDone') : t('item.markDone')"
          :aria-label="todo.done ? t('item.markNotDone') : t('item.markDone')"
          @click="emit('toggleDone')"
        >
          <Check class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="outline"
          size="icon-sm"
          class="shrink-0"
          :title="t('item.editTitle')"
          :aria-label="t('item.edit')"
          @click="emit('edit')"
        >
          <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <CopyMarkdownButton v-if="note" icon-only class="shrink-0" :text="note.content" />
        <DetailActionsMenu
          :new-session-label="newSessionLabel"
          :new-session-hint="item.project_id ? t('item.newSessionHint') : t('item.needProject')"
          :can-new-session="!!item.project_id"
          :copied-id="copiedId"
          @new-session="emit('newSession')"
          @copy-id="emit('copyId')"
          @delete="emit('delete')"
        />
      </template>
    </div>
  </div>
</template>
