<script setup lang="ts">
// Settings → Sync → Projects → ⋯: one project's folder-sync settings on this machine — local folder,
// excludes (defaults + extra gitignore-style patterns), max file size, last backup. Saved with
// `sync_project_pref_set` (only the changed fields).
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openPath } from '@tauri-apps/plugin-opener'
import { FolderOpen } from 'lucide-vue-next'
import { Button, Input, Modal, Textarea } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import {
  DEFAULT_EXCLUDES,
  ageParts,
  backupDir,
  excludesToText,
  parseExcludes,
  parseMaxFileMb,
  parseTimestamp,
  syncErrorCode,
  syncProjectPrefSet,
  type ProjectPref,
  type ProjectPrefPatch,
} from '@/lib/sync'

const props = defineProps<{ project: ProjectPref | null }>()
const emit = defineEmits<{ close: []; saved: [] }>()

const { t } = useI18n()
const { toast } = useToast()

const localPath = ref('')
const defaultExcludes = ref(true)
const extra = ref('')
const maxMb = ref('')
const saving = ref(false)

watch(
  () => props.project,
  (p) => {
    if (!p) return
    localPath.value = p.local_path ?? ''
    defaultExcludes.value = p.default_excludes
    extra.value = excludesToText(p.excludes)
    maxMb.value = String(p.max_file_mb)
  },
  { immediate: true },
)

const maxValid = computed(() => parseMaxFileMb(maxMb.value) !== null)
const lastBackup = computed(() => parseTimestamp(props.project?.last_backup_at))

function ago(ms: number): string {
  const { unit, n } = ageParts(ms)
  return t(`settings.sync.ago.${unit}`, { n })
}

async function pick() {
  const selected = await openDialog({ directory: true, multiple: false, title: props.project?.name })
  if (selected && !Array.isArray(selected)) localPath.value = selected
}

async function openBackup() {
  const p = props.project?.local_path
  if (p) await openPath(backupDir(p)).catch(() => toast.error(backupDir(p)))
}

async function save() {
  const p = props.project
  const max = parseMaxFileMb(maxMb.value)
  if (!p || saving.value || max === null) return
  const patch: ProjectPrefPatch = {}
  const path = localPath.value.trim()
  if (path && path !== (p.local_path ?? '')) patch.local_path = path
  if (defaultExcludes.value !== p.default_excludes) patch.default_excludes = defaultExcludes.value
  const excludes = parseExcludes(extra.value)
  if (excludes.join('\n') !== p.excludes.join('\n')) patch.excludes = excludes
  if (max !== p.max_file_mb) patch.max_file_mb = max
  saving.value = true
  try {
    if (Object.keys(patch).length) await syncProjectPrefSet(p.project_id, patch)
    toast.success(t('settings.sync.projectDialog.saved'))
    emit('saved')
    emit('close')
  } catch (e) {
    const code = syncErrorCode(e)
    toast.error(code ? t(`settings.sync.errors.${code}`) : String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Modal
    :open="!!project"
    size="md"
    :dismiss-on-overlay="false"
    :title="project ? t('settings.sync.projectDialog.title', { name: project.name }) : ''"
    @close="emit('close')"
  >
    <div v-if="project" class="space-y-4 p-4">
      <section class="space-y-1.5">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
          {{ t('settings.sync.projectDialog.localPath') }}
        </label>
        <div class="flex items-center gap-2">
          <Input v-model="localPath" class="font-mono" />
          <Button variant="outline" size="sm" @click="pick">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('settings.sync.projectDialog.choose') }}
          </Button>
        </div>
      </section>

      <section class="space-y-2">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
          {{ t('settings.sync.projectDialog.excludes') }}
        </label>
        <label class="flex cursor-pointer items-start gap-2 text-xs">
          <input v-model="defaultExcludes" type="checkbox" class="mt-0.5 accent-primary">
          <span class="text-muted-foreground">
            {{ t('settings.sync.projectDialog.defaultExcludes', { list: DEFAULT_EXCLUDES.join(', ') }) }}
          </span>
        </label>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.projectDialog.extra') }}</p>
        <Textarea v-model="extra" rows="4" class="font-mono" placeholder="*.log&#10;tmp/" />
      </section>

      <section class="space-y-1">
        <div class="flex items-center gap-2 text-xs">
          <span>{{ t('settings.sync.projectDialog.maxSize') }}</span>
          <Input v-model="maxMb" class="w-24 tabular-nums" inputmode="numeric" />
          <span class="text-muted-foreground">MB</span>
        </div>
        <p v-if="!maxValid" class="text-[11px] text-destructive">{{ t('settings.sync.projectDialog.maxInvalid') }}</p>
      </section>

      <section class="flex flex-wrap items-center gap-2 text-xs">
        <span class="text-muted-foreground">
          {{ lastBackup !== null ? t('settings.sync.projectDialog.lastBackup', { ago: ago(lastBackup) })
            : t('settings.sync.projectDialog.lastBackupNever') }}
        </span>
        <Button v-if="project.local_path && project.exists" variant="ghost" size="xs" @click="openBackup">
          <FolderOpen class="h-3 w-3" :stroke-width="1.75" />
          {{ t('settings.sync.projectDialog.openBackup') }}
        </Button>
      </section>
    </div>

    <template #footer>
      <Button variant="outline" size="sm" @click="emit('close')">{{ t('settings.sync.projectDialog.cancel') }}</Button>
      <Button size="sm" :disabled="saving || !maxValid" @click="save">{{ t('settings.sync.projectDialog.save') }}</Button>
    </template>
  </Modal>
</template>
