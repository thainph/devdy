<script setup lang="ts">
/**
 * Note list of the phone Controller (spec §2.B2): search, a plain-text
 * preview per note and a floating + for a new one. The project filter is the
 * tab header's dropdown (the `projectId` prop).
 *
 * Search runs on the Host (title + content), so the filter goes out as a
 * debounced `filter` event and the matching notes come back in via `notes`.
 * The current filter is a prop so it survives opening a note and coming back.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Plus, X } from 'lucide-vue-next'
import { relativeAge } from '@/lib/itemText'
import { noteDisplayTitle, noteListPreview } from '@/lib/remoteTodos'
import SearchField from './SearchField.vue'
import type { NoteSummary, ProjectInfo } from '../protocol'

const props = defineProps<{
  /** null until the Host answers. */
  notes: NoteSummary[] | null
  /** The last list request failed (refused / no answer). */
  failed?: boolean
  projects: ProjectInfo[]
  /** The filter last asked for. */
  projectId: string | null
  query: string
  error: string | null
}>()

const emit = defineEmits<{
  /** Ask the Host for notes matching this filter. */
  filter: [projectId: string | null, query: string]
  open: [note: NoteSummary]
  new: []
  'clear-error': []
}>()

const { t } = useI18n()

const search = ref(props.query)

onMounted(() => emit('filter', props.projectId, search.value))

// Typing is debounced so a word costs one round trip, not one per keystroke.
let timer: ReturnType<typeof setTimeout> | null = null
watch(search, (q) => {
  if (timer) clearTimeout(timer)
  timer = setTimeout(() => emit('filter', props.projectId, q), 300)
})
onBeforeUnmount(() => {
  if (timer) clearTimeout(timer)
})

const projectNames = computed(() => new Map(props.projects.map((p) => [p.id, p.name])))
const searching = computed(() => search.value.trim().length > 0)
/** The Host sends the start of the body; flatten it for one or two lines. */
function previewOf(note: NoteSummary): string {
  return noteListPreview({ title: note.title, content: note.preview })
}
</script>

<template>
  <div class="relative flex h-full min-h-0 flex-col bg-background text-foreground">

    <div
      v-if="error"
      class="shrink-0 flex items-start gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive"
    >
      <span class="flex-1">{{ error }}</span>
      <button class="shrink-0 cursor-pointer" :title="t('controller.data.dismiss')" @click="emit('clear-error')">
        <X class="h-3.5 w-3.5" :stroke-width="2" />
      </button>
    </div>

    <SearchField
      v-model="search"
      :placeholder="t('controller.notes.searchPlaceholder')"
      :clear-label="t('controller.notes.clearSearch')"
    />

    <main class="flex-1 min-h-0 overflow-auto px-3 pt-3 pb-24">
      <div v-if="notes === null && failed" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <button
          class="rounded-md border border-border bg-secondary px-3 py-1.5 text-xs text-foreground hover:bg-accent cursor-pointer"
          @click="emit('filter', projectId, search)"
        >
          {{ t('controller.data.retry') }}
        </button>
      </div>
      <p v-else-if="notes === null" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>
      <p v-else-if="!notes.length" class="py-10 text-center text-sm text-foreground/50">
        {{ searching ? t('controller.notes.noResults') : t('controller.notes.empty') }}
      </p>
      <div v-else class="overflow-hidden rounded-xl border border-border bg-card">
        <button
          v-for="note in notes"
          :key="note.id"
          class="block w-full px-3 py-2.5 text-left border-b border-border/60 last:border-b-0 hover:bg-accent/50 transition-colors cursor-pointer"
          @click="emit('open', note)"
        >
          <span class="flex items-baseline gap-2">
            <span class="min-w-0 flex-1 truncate text-sm font-medium">
              {{ noteDisplayTitle({ title: note.title, content: note.preview }) || t('controller.notes.untitled') }}
            </span>
            <span class="shrink-0 text-[10px] text-muted-foreground">{{ relativeAge(note.updated_at) }}</span>
          </span>
          <span v-if="previewOf(note)" class="mt-0.5 block line-clamp-2 text-xs text-muted-foreground">
            {{ previewOf(note) }}
          </span>
          <span
            v-if="note.project_id && projectNames.get(note.project_id)"
            class="mt-1.5 inline-block rounded-full border border-border bg-secondary px-1.5 py-px text-[10px] text-muted-foreground"
          >
            {{ projectNames.get(note.project_id) }}
          </span>
        </button>
      </div>
    </main>

    <button
      class="absolute right-5 bottom-5 z-10 grid h-14 w-14 place-items-center rounded-full bg-primary text-primary-foreground shadow-lg shadow-primary/40 active:scale-95 transition cursor-pointer"
      :title="t('controller.notes.add')"
      :aria-label="t('controller.notes.add')"
      @click="emit('new')"
    >
      <Plus class="h-6 w-6" :stroke-width="2.25" />
    </button>
  </div>
</template>
