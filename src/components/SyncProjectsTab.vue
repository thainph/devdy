<script setup lang="ts">
// Settings → Sync → Projects: per project on this machine — what it receives (nothing / data /
// data + folder), its folder here (present / will be created / pick one; click to choose), and ⋯
// for excludes / max size / backups (SyncProjectDialog). "Set all" applies one mode to every
// project. Machine-local (`sync_prefs_get` / `sync_project_pref_set`).
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { open } from '@tauri-apps/plugin-dialog'
import { FolderOpen, Loader2, MoreHorizontal } from 'lucide-vue-next'
import { AppSelect, Button } from '@/components/ui'
import SyncProjectDialog from '@/components/SyncProjectDialog.vue'
import { useToast } from '@/composables/useToast'
import { useSyncStore } from '@/stores/sync'
import {
  PROJECT_MODES,
  commonMode,
  isRemapped,
  localFolderState,
  modePatch,
  projectMode,
  syncErrorCode,
  syncProjectPrefSet,
  type LocalFolderState,
  type ProjectMode,
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

const modeOptions = computed(() => PROJECT_MODES.map((m) => ({ value: m, label: t(`settings.sync.projects.mode.${m}`) })))
const allMode = computed(() => commonMode(store.prefs?.projects ?? []) ?? '')

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

function setMode(p: ProjectPref, mode: string) {
  if (mode !== projectMode(p)) void patch(p, modePatch(mode as ProjectMode))
}

/** Apply `mode` to every project not in it yet. */
async function setAll(mode: string) {
  if (busy.value || !store.prefs || !mode) return
  busy.value = 'all'
  try {
    for (const p of store.prefs.projects) {
      if (projectMode(p) !== mode) await syncProjectPrefSet(p.project_id, modePatch(mode as ProjectMode))
    }
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    await store.loadPrefs().catch(() => {})
    busy.value = null
  }
}

async function pickFolder(p: ProjectPref) {
  if (busy.value) return
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
    <div v-if="store.prefs?.projects.length" class="flex items-center gap-2">
      <span class="shrink-0 text-xs text-muted-foreground">{{ t('settings.sync.projects.setAll') }}</span>
      <AppSelect
        class="w-44"
        size="xs"
        :model-value="allMode"
        :options="modeOptions"
        :placeholder="t('settings.sync.projects.mixed')"
        :disabled="busy !== null"
        @update:model-value="setAll"
      />
      <Loader2 v-if="busy === 'all'" class="h-3.5 w-3.5 animate-spin text-muted-foreground" :stroke-width="1.75" />
    </div>
    <p v-if="store.prefs && !store.prefs.projects.length" class="text-[11px] text-muted-foreground">
      {{ t('settings.sync.projects.empty') }}
    </p>
    <ul v-else-if="store.prefs" class="divide-y divide-border/60 rounded-lg border border-border/60">
      <li v-for="p in store.prefs.projects" :key="p.project_id" class="flex items-center gap-2 px-3 py-2">
        <div class="min-w-0 flex-1 space-y-0.5">
          <p class="flex min-w-0 items-baseline gap-2">
            <span class="max-w-[60%] shrink-0 truncate text-sm" :class="{ 'text-muted-foreground': !p.enabled }">{{ p.name }}</span>
            <span class="min-w-0 truncate font-mono text-[10px] text-muted-foreground/70" :title="p.synced_path">{{ p.synced_path }}</span>
          </p>
          <button
            type="button"
            class="flex w-full min-w-0 items-center gap-1.5 text-left text-[11px] text-muted-foreground hover:text-foreground disabled:pointer-events-none"
            :title="p.local_path || t('settings.sync.projects.pick')"
            :disabled="busy !== null"
            @click="pickFolder(p)"
          >
            <component
              :is="busy === p.project_id ? Loader2 : FolderOpen"
              class="h-3 w-3 shrink-0"
              :class="{ 'animate-spin': busy === p.project_id }"
              :stroke-width="1.75"
            />
            <span v-if="p.local_path" class="min-w-0 truncate font-mono">{{ p.local_path }}</span>
            <span class="shrink-0" :class="STATE_CLASS[localFolderState(p)]">
              {{ t(`settings.sync.projects.state.${localFolderState(p)}`) }}
            </span>
            <span v-if="isRemapped(p)" class="shrink-0">· {{ t('settings.sync.projects.remapped') }}</span>
          </button>
          <p v-if="p.running_here" class="text-[11px] text-muted-foreground">{{ t('settings.sync.projects.runningHere') }}</p>
        </div>
        <AppSelect
          class="w-36 shrink-0"
          size="xs"
          :model-value="projectMode(p)"
          :options="modeOptions"
          :disabled="busy !== null"
          @update:model-value="setMode(p, $event)"
        />
        <Button variant="ghost" size="icon-sm" class="shrink-0" :title="t('settings.sync.projects.more')" @click="editing = p">
          <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </li>
    </ul>
    <p v-else class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
      <Loader2 class="h-3 w-3 animate-spin" :stroke-width="1.75" />
    </p>

    <SyncProjectDialog :project="editing" @close="editing = null" @saved="reload" />
  </section>
</template>
