<script setup lang="ts">
// Header of the Secret notes window's detail pane — the sibling of ItemHeader,
// same layout and controls:
//
//   title (click → edit) / updated · encrypted in the vault
//   right [Project ▾] | 👁 Reveal / Hide · ⧉ Copy · ✎ Edit · ⋯ (Delete)
//
// While editing (or writing a new note), the actions give way to Cancel · Save
// (⌘/Ctrl+Enter / Esc do the same inside the fields).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Copy, Eye, EyeOff, Loader2, MoreHorizontal, Pencil, Trash2 } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate } from '@/lib/slackThreadUtils'
import type { SecretNoteMeta } from '@/lib/secretVault'

const props = defineProps<{
  /** The selected note; null while writing a new one. */
  note: SecretNoteMeta | null
  /** The project shown: the note's, or the new note's. */
  projectId: string | null
  editing: boolean
  canSave: boolean
  saving: boolean
  /** The content is on screen. */
  revealed: boolean
  revealing: boolean
}>()

const emit = defineEmits<{
  edit: []
  cancelEdit: []
  save: []
  setProject: [projectId: string | null]
  reveal: []
  hide: []
  copy: []
  delete: []
}>()

const { t } = useI18n()

const heading = computed(() =>
  props.note ? props.note.title.trim() || t('secretNotes.untitled') : t('secretNotes.newTitle'),
)

const meta = computed(() =>
  props.note
    ? [t('item.metaUpdated', { age: relativeAge(props.note.updated_at) }), t('secretNotes.metaEncrypted')].join(' · ')
    : t('secretNotes.newMeta'),
)

const metaTitle = computed(() => {
  const n = props.note
  if (!n) return ''
  const lines = [t('item.createdAt', { date: formatFullDate(n.created_at) })]
  if (n.updated_at !== n.created_at) lines.push(t('item.updatedAt', { date: formatFullDate(n.updated_at) }))
  return lines.join('\n')
})
</script>

<template>
  <div class="flex shrink-0 items-center gap-3 border-b border-border/60 px-5 py-2.5">
    <div class="min-w-[6rem] flex-1">
      <h1
        class="min-w-0 truncate rounded px-0.5 -mx-0.5 text-[15px] font-bold leading-snug"
        :class="note && !editing && 'cursor-text transition-colors hover:bg-accent/50'"
        :title="note && !editing ? `${heading}\n${t('item.editTitle')}` : heading"
        @click="note && !editing && emit('edit')"
      >
        {{ heading }}
      </h1>
      <p class="mt-0.5 truncate text-xs text-muted-foreground" :title="metaTitle">{{ meta }}</p>
    </div>

    <div class="flex min-w-0 items-center gap-1.5">
      <div class="w-[11rem] min-w-[5.5rem] shrink">
        <ProjectSelect
          :model-value="projectId"
          variant="outline"
          size="xs"
          @update:model-value="emit('setProject', $event)"
        />
      </div>
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
          <Loader2 v-if="saving" class="h-3 w-3 animate-spin" :stroke-width="1.75" />
          {{ t('common.save') }}
        </Button>
      </template>
      <template v-else-if="note">
        <Button
          :variant="revealed ? 'primary' : 'outline'"
          size="icon-sm"
          class="shrink-0"
          :aria-pressed="revealed"
          :disabled="revealing"
          :title="revealed ? t('secretNotes.hide') : t('secretNotes.reveal')"
          :aria-label="revealed ? t('secretNotes.hide') : t('secretNotes.reveal')"
          @click="revealed ? emit('hide') : emit('reveal')"
        >
          <Loader2 v-if="revealing" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
          <component :is="revealed ? EyeOff : Eye" v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="outline"
          size="icon-sm"
          class="shrink-0"
          :title="t('secretNotes.copy')"
          :aria-label="t('secretNotes.copy')"
          @click="emit('copy')"
        >
          <Copy class="h-3.5 w-3.5" :stroke-width="1.75" />
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
        <DropdownMenu align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-sm"
              :title="t('item.moreActions')"
              :aria-label="t('item.moreActions')"
            >
              <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <DropdownItem variant="destructive" @click="emit('delete')">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.delete') }}
          </DropdownItem>
        </DropdownMenu>
      </template>
    </div>
  </div>
</template>
