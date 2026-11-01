<script setup lang="ts">
/**
 * One note on the phone Controller (spec §2.B2): View renders the Markdown,
 * Edit is a plain title / project / Markdown form with Delete and Save. A new
 * note (`noteId` null) opens straight in Edit; an existing one is fetched in
 * full (`note_get` — the list only has previews) and shows a spinner meanwhile.
 *
 * Writes go through the `save` / `remove` props so the editor only moves on
 * once the Host accepted them. A save sends only the fields changed since the
 * user entered Edit. A new note stays open under the id the Host's ack carries.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, Loader2, X } from 'lucide-vue-next'
import { AppSelect, Banner, Button, Input, Textarea } from '@/components/ui'
import { noteDisplayTitle } from '@/lib/remoteTodos'
import ControllerHeader from './ControllerHeader.vue'
import HeaderButton from './HeaderButton.vue'
import type { NoteInfo, ProjectInfo } from '../protocol'
import type { NoteSave } from '../dataStore'

const props = defineProps<{
  /** The note being shown; null while it loads, or for a new note. */
  note: NoteInfo | null
  /** Its id; null for a new note. */
  noteId: string | null
  /** Fetching the note failed (refused / no answer). */
  failed: boolean
  projects: ProjectInfo[]
  /** Project a new note starts in ('' = none). */
  defaultProjectId: string
  busy: boolean
  error: string | null
  renderText: (md: string) => string
  /** Resolves to the note as stored, or null when refused. */
  save: (input: NoteSave) => Promise<NoteInfo | null>
  remove: (id: string) => Promise<boolean>
}>()

const emit = defineEmits<{
  /** Fetch the note again after a failure. */
  retry: []
  back: []
  'clear-error': []
}>()

const { t } = useI18n()

/** What the note holds on the Host as far as we know (updated on save). */
const shown = ref({ title: '', content: '', projectId: '' })
const mode = ref<'view' | 'edit'>('view')
const title = ref('')
const content = ref('')
const projectId = ref('')
const confirmingDelete = ref(false)
const localError = ref<string | null>(null)
/** The note as it was when the user entered Edit: what a save diffs against. */
const base = ref<NoteInfo | null>(null)

const isNew = computed(() => props.noteId === null)
const loading = computed(() => !isNew.value && !props.note && !props.failed)

watch(
  () => props.note,
  (note, prev) => {
    shown.value = {
      title: note?.title ?? '',
      content: note?.content ?? '',
      projectId: note ? (note.project_id ?? '') : props.defaultProjectId,
    }
    // A refresh of the note being edited (a reconnect re-fetches it) must not
    // wipe the draft.
    if (note && prev && note.id === prev.id && mode.value === 'edit') return
    mode.value = isNew.value && !note ? 'edit' : 'view'
    base.value = null
    resetForm()
  },
  { immediate: true },
)

function resetForm(): void {
  title.value = shown.value.title
  content.value = shown.value.content
  projectId.value = shown.value.projectId
  confirmingDelete.value = false
  localError.value = null
}

function setMode(next: 'view' | 'edit'): void {
  if (next === mode.value) return
  // Leaving Edit without saving drops the draft, like Cancel.
  if (next === 'view') resetForm()
  else base.value = props.note
  mode.value = next
}

const projectOptions = computed(() => [
  { value: '', label: t('controller.data.noProject') },
  ...props.projects.map((p) => ({ value: p.id, label: p.name })),
])

const heading = computed(() =>
  !isNew.value
    ? noteDisplayTitle(shown.value) || t('controller.notes.untitled')
    : t('controller.notes.newNote'),
)
const projectName = computed(() => props.projects.find((p) => p.id === shown.value.projectId)?.name ?? '')
const body = computed(() => {
  // The title is the heading already; render it on top only when it is not
  // repeated as the first line of the body.
  const { title: t0, content: c } = shown.value
  return t0.trim() && !c.trimStart().startsWith(`# ${t0.trim()}`) ? `# ${t0.trim()}\n\n${c}` : c
})

async function submit(): Promise<void> {
  if (props.busy) return
  if (!title.value.trim() && !content.value.trim()) {
    localError.value = t('controller.notes.needsContent')
    return
  }
  localError.value = null
  const wasNew = isNew.value
  const stored = await props.save({
    id: props.noteId,
    title: title.value,
    content: content.value,
    projectId: projectId.value,
    base: base.value,
  })
  if (!stored) return
  // A new note stays open under its new id (the parent's store adopts it); an
  // id-less ack from an older Host can only send us back to the list.
  if (wasNew && !stored.id) {
    emit('back')
    return
  }
  base.value = null
  shown.value = { title: stored.title, content: stored.content, projectId: stored.project_id ?? '' }
  resetForm()
  mode.value = 'view'
}

async function onDelete(): Promise<void> {
  if (!props.note || props.busy) return
  if (!confirmingDelete.value) {
    confirmingDelete.value = true
    return
  }
  if (await props.remove(props.note.id)) emit('back')
}

const banner = computed(() => localError.value ?? props.error)
function clearBanner(): void {
  localError.value = null
  emit('clear-error')
}
</script>

<template>
  <div class="relative flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="heading">
      <template #leading>
        <HeaderButton class="-ml-1.5" :label="t('controller.data.back')" @click="emit('back')">
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </HeaderButton>
      </template>
      <div v-if="note" class="flex shrink-0 gap-0.5 rounded-lg border border-border bg-card p-0.5">
        <button
          v-for="m in (['view', 'edit'] as const)"
          :key="m"
          class="rounded-md px-2.5 py-1 text-xs transition-colors cursor-pointer"
          :class="mode === m ? 'bg-secondary font-semibold text-foreground' : 'text-muted-foreground'"
          @click="setMode(m)"
        >
          {{ t(`controller.notes.${m}`) }}
        </button>
      </div>
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
      <p v-if="loading" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>
      <div v-else-if="!isNew && !note" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <Button variant="outline" size="sm" @click="emit('retry')">{{ t('controller.data.retry') }}</Button>
      </div>
      <template v-else-if="mode === 'view'">
        <span
          v-if="projectName"
          class="mb-2 inline-block rounded-full border border-border bg-secondary px-1.5 py-px text-[10px] text-muted-foreground"
        >
          {{ projectName }}
        </span>
        <div v-if="body.trim()" class="markdown-output text-sm leading-relaxed text-foreground" v-html="renderText(body)" />
        <p v-else class="py-10 text-center text-sm text-foreground/50">{{ t('controller.notes.emptyBody') }}</p>
      </template>

      <form v-else class="flex min-h-full flex-col" @submit.prevent="submit">
        <label class="mb-3 block">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.notes.titleLabel') }}</span>
          <Input
            v-model="title"
            size="md"
            class="w-full"
            autocomplete="off"
            :placeholder="t('controller.notes.titlePlaceholder')"
          />
        </label>
        <div class="mb-3">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.data.project') }}</span>
          <AppSelect v-model="projectId" size="md" class="w-full" :options="projectOptions" />
        </div>
        <label class="mb-3 flex flex-1 flex-col">
          <span class="mb-1 block text-[11px] text-muted-foreground">{{ t('controller.notes.contentLabel') }}</span>
          <Textarea
            v-model="content"
            size="md"
            class="min-h-[16rem] w-full flex-1 resize-none font-mono text-[13px]"
            :placeholder="t('controller.notes.contentPlaceholder')"
          />
        </label>
        <div class="grid grid-cols-2 gap-2 pb-[env(safe-area-inset-bottom)]">
          <Button v-if="note" variant="destructive" size="md" :disabled="busy" @click="onDelete">
            {{ confirmingDelete ? t('controller.notes.confirmDelete') : t('controller.notes.delete') }}
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
