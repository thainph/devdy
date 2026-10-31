<script setup lang="ts">
/**
 * One note on the phone Controller (spec §2.B2): View renders the Markdown,
 * Edit is a plain title / project / Markdown form with Delete and Save. A new
 * note (`note` null) opens straight in Edit.
 *
 * Writes go through the `save` / `remove` props so the editor only moves on
 * once the Host accepted them. After saving an existing note it shows what was
 * saved; a new note is re-opened under the id the Host's ack carries.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, X } from 'lucide-vue-next'
import { AppSelect, Button, Input, Textarea } from '@/components/ui'
import { noteDisplayTitle } from '@/lib/remoteTodos'
import type { NoteInfo, ProjectInfo } from '../protocol'
import type { NoteSave } from '../dataStore'

const props = defineProps<{
  /** The note to show; null for a new one. */
  note: NoteInfo | null
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
  /** A new note was saved; show it. */
  created: [note: NoteInfo]
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

watch(
  () => props.note,
  (note) => {
    shown.value = {
      title: note?.title ?? '',
      content: note?.content ?? '',
      projectId: note ? (note.project_id ?? '') : props.defaultProjectId,
    }
    mode.value = note ? 'view' : 'edit'
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
  mode.value = next
}

const projectOptions = computed(() => [
  { value: '', label: t('controller.data.noProject') },
  ...props.projects.map((p) => ({ value: p.id, label: p.name })),
])

const heading = computed(() =>
  props.note
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
  const stored = await props.save({
    id: props.note?.id ?? null,
    title: title.value,
    content: content.value,
    projectId: projectId.value,
  })
  if (!stored) return
  if (!props.note) {
    // A new note: the parent re-opens the editor on it (in View).
    if (stored.id) emit('created', stored)
    else emit('back')
    return
  }
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
    <header class="shrink-0 flex items-center gap-2 px-3 py-2 border-b border-border">
      <button
        class="inline-flex items-center justify-center h-7 w-7 -ml-1 shrink-0 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
        :title="t('controller.data.back')"
        @click="emit('back')"
      >
        <ChevronLeft class="h-4 w-4" :stroke-width="2" />
      </button>
      <h1 class="min-w-0 flex-1 truncate text-sm font-semibold">{{ heading }}</h1>
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
    </header>

    <div
      v-if="banner"
      class="shrink-0 flex items-start gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive"
    >
      <span class="flex-1">{{ banner }}</span>
      <button class="shrink-0 cursor-pointer" :title="t('controller.data.dismiss')" @click="clearBanner">
        <X class="h-3.5 w-3.5" :stroke-width="2" />
      </button>
    </div>

    <main class="flex-1 min-h-0 overflow-auto px-3 py-3">
      <template v-if="mode === 'view'">
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
