<script setup lang="ts">
// Settings → Sync → Projects: per project on this machine — receive it (Sync), its folder here
// (present / will be created / pick one), folder sync on/off, and ⋯ for excludes / max size /
// backups (SyncProjectDialog). Machine-local (`sync_prefs_get` / `sync_project_pref_set`).
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { open } from '@tauri-apps/plugin-dialog'
import { FolderOpen, Loader2, MoreHorizontal } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import SyncProjectDialog from '@/components/SyncProjectDialog.vue'
import { useToast } from '@/composables/useToast'
import { useSyncStore } from '@/stores/sync'
import {
  isRemapped,
  localFolderState,
  syncErrorCode,
  syncProjectPrefSet,
  type LocalFolderState,
  type ProjectPref,
  type ProjectPrefPatch,
} from '@/lib/sync'

const { t } = useI18n()
const { toast } = useToast()
const store = useSyncStore()

const busy = ref<string | null>(null)
const editing = ref<ProjectPref | null>(null)

const STATE_CLASS: Record<LocalFolderState, string> = {
  ok: 'text-emerald-600 dark:text-emerald-400',
  create: 'text-sky-600 dark:text-sky-400',
  pick: 'text-amber-600 dark:text-amber-400',
}

function errorText(e: unknown): string {
  const code = syncErrorCode(e)
  return code ? t(`settings.sync.errors.${code}`) : String(e)
}

async function patch(p: ProjectPref, change: ProjectPrefPatch) {
  if (busy.value) return
  busy.value = p.project_id
  try {
    await syncProjectPrefSet(p.project_id, change)
    await store.loadPrefs()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    busy.value = null
  }
}

/** Turn `key` on for every project still off (folder sync needs a local path). */
async function selectAll(key: 'enabled' | 'folder') {
  if (busy.value || !store.prefs) return
  busy.value = 'all'
  try {
    for (const p of store.prefs.projects) {
      if (p[key] || (key === 'folder' && !p.local_path)) continue
      await syncProjectPrefSet(p.project_id, key === 'folder' ? { enabled: true, folder: true } : { enabled: true })
    }
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    await store.loadPrefs().catch(() => {})
    busy.value = null
  }
}

async function pickFolder(p: ProjectPref) {
  const selected = await open({ directory: true, multiple: false, title: p.name })
  if (!selected || Array.isArray(selected)) return
  await patch(p, { local_path: selected })
}

async function reload() {
  await store.loadPrefs().catch((e: unknown) => toast.error(errorText(e)))
}

onMounted(reload)
</script>

<template>
  <section class="space-y-3">
    <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.projects.tabHelp') }}</p>
    <div v-if="store.prefs?.projects.length" class="flex flex-wrap items-center gap-2">
      <Button variant="outline" size="sm" :disabled="busy !== null" @click="selectAll('enabled')">
        <Loader2 v-if="busy === 'all'" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ t('settings.sync.projects.selectAll') }}
      </Button>
      <Button variant="outline" size="sm" :disabled="busy !== null" @click="selectAll('folder')">
        {{ t('settings.sync.projects.selectAllFolders') }}
      </Button>
    </div>
    <p v-if="store.prefs && !store.prefs.projects.length" class="text-[11px] text-muted-foreground">
      {{ t('settings.sync.projects.empty') }}
    </p>
    <div v-else-if="store.prefs" class="overflow-x-auto rounded-lg border border-border/60">
      <table class="w-full text-xs">
        <thead class="border-b border-border/60 bg-muted/20 text-[10px] uppercase tracking-wider text-muted-foreground">
          <tr>
            <th class="w-12 px-3 py-2 text-left font-medium">{{ t('settings.sync.projects.colSync') }}</th>
            <th class="px-3 py-2 text-left font-medium">{{ t('settings.sync.projects.colProject') }}</th>
            <th class="px-3 py-2 text-left font-medium">{{ t('settings.sync.projects.colLocal') }}</th>
            <th class="w-16 px-3 py-2 text-center font-medium">{{ t('settings.sync.projects.colFolder') }}</th>
            <th class="w-10 px-2 py-2" />
          </tr>
        </thead>
        <tbody class="divide-y divide-border/60">
          <tr v-for="p in store.prefs.projects" :key="p.project_id" class="align-top">
            <td class="px-3 py-2">
              <input
                type="checkbox"
                class="accent-primary"
                :checked="p.enabled"
                :disabled="busy !== null"
                :aria-label="t('settings.sync.projects.colSync')"
                @change="patch(p, { enabled: ($event.target as HTMLInputElement).checked })"
              >
            </td>
            <td class="max-w-[12rem] px-3 py-2">
              <p class="truncate text-sm">{{ p.name }}</p>
              <p class="truncate font-mono text-[11px] text-muted-foreground" :title="p.synced_path">{{ p.synced_path }}</p>
            </td>
            <td class="max-w-[16rem] px-3 py-2">
              <div class="flex items-start gap-2">
                <div class="min-w-0 flex-1">
                  <p v-if="p.local_path" class="truncate font-mono text-[11px]" :title="p.local_path">{{ p.local_path }}</p>
                  <p class="text-[11px]" :class="STATE_CLASS[localFolderState(p)]">
                    {{ t(`settings.sync.projects.state.${localFolderState(p)}`) }}
                    <span v-if="isRemapped(p)" class="text-muted-foreground"> · {{ t('settings.sync.projects.remapped') }}</span>
                  </p>
                  <p v-if="p.running_here" class="text-[11px] text-muted-foreground">{{ t('settings.sync.projects.runningHere') }}</p>
                </div>
                <Button variant="outline" size="xs" :disabled="busy !== null" @click="pickFolder(p)">
                  <component
                    :is="busy === p.project_id ? Loader2 : FolderOpen"
                    class="h-3 w-3"
                    :class="{ 'animate-spin': busy === p.project_id }"
                    :stroke-width="1.75"
                  />
                  {{ t('settings.sync.projects.pick') }}
                </Button>
              </div>
            </td>
            <td class="px-3 py-2 text-center">
              <input
                type="checkbox"
                class="accent-primary"
                :checked="p.folder"
                :disabled="busy !== null || !p.enabled"
                :title="p.enabled ? t('settings.sync.projects.colFolder') : t('settings.sync.projects.folderNeedsSync')"
                :aria-label="t('settings.sync.projects.colFolder')"
                @change="patch(p, { folder: ($event.target as HTMLInputElement).checked })"
              >
            </td>
            <td class="px-2 py-1.5">
              <Button variant="ghost" size="xs" :title="t('settings.sync.projects.more')" @click="editing = p">
                <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p v-else class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
      <Loader2 class="h-3 w-3 animate-spin" :stroke-width="1.75" />
    </p>

    <SyncProjectDialog :project="editing" @close="editing = null" @saved="reload" />
  </section>
</template>
