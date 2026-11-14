<script setup lang="ts">
// Project overview card for per-task worktrees: the base branch and setup
// command new worktrees use, plus the active ones (to clean them up).
import { onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { FolderGit2, Trash2 } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { Button, Card, Input } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'

interface Worktree {
  id: string
  path: string
  branch: string
  baseRef: string
  createdAt: string
}
interface WorktreeSettings {
  baseBranch: string | null
  setupCmd: string | null
}

const props = defineProps<{ projectId: string }>()
const { t } = useI18n()
const { confirm } = useConfirm()
const { toast } = useToast()

const baseBranch = ref('')
const setupCmd = ref('')
let saved: WorktreeSettings = { baseBranch: null, setupCmd: null }
const worktrees = ref<Worktree[]>([])

async function load() {
  const [s, list] = await Promise.all([
    invoke<WorktreeSettings>('get_project_worktree_settings', { projectId: props.projectId }),
    invoke<Worktree[]>('worktrees_list', { projectId: props.projectId }),
  ])
  saved = s
  baseBranch.value = s.baseBranch ?? ''
  setupCmd.value = s.setupCmd ?? ''
  worktrees.value = list
}

/** Saves on blur, only when something changed. */
async function save() {
  const next: WorktreeSettings = {
    baseBranch: baseBranch.value.trim() || null,
    setupCmd: setupCmd.value.trim() || null,
  }
  if (next.baseBranch === saved.baseBranch && next.setupCmd === saved.setupCmd) return
  try {
    await invoke('set_project_worktree_settings', { projectId: props.projectId, settings: next })
    saved = next
    toast.success(t('projectDetail.worktrees.saved'))
  } catch (e) {
    toast.error(String(e))
  }
}

async function remove(wt: Worktree) {
  if (!(await confirm({
    title: t('projectDetail.worktrees.removeTitle'),
    message: t('projectDetail.worktrees.removeMessage', { branch: wt.branch }),
    confirmLabel: t('common.delete'),
  }))) return
  try {
    await invoke('worktree_remove', { id: wt.id, deleteBranch: true })
    worktrees.value = worktrees.value.filter((w) => w.id !== wt.id)
  } catch (e) {
    toast.error(String(e))
  }
}

onMounted(() => void load().catch((e) => toast.error(String(e))))
watch(() => props.projectId, () => void load().catch((e) => toast.error(String(e))))
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <template #header>
      <FolderGit2 class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('projectDetail.worktrees.title') }}</span>
    </template>
    <p class="text-[11px] text-muted-foreground">{{ t('projectDetail.worktrees.hint') }}</p>
    <div class="space-y-1.5">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('projectDetail.worktrees.baseBranch') }}</label>
      <Input v-model="baseBranch" size="sm" :placeholder="t('projectDetail.worktrees.baseBranchPlaceholder')" @blur="save" />
    </div>
    <div class="space-y-1.5">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('projectDetail.worktrees.setupCmd') }}</label>
      <Input v-model="setupCmd" size="sm" class="font-mono" placeholder="pnpm install" @blur="save" />
      <p class="text-[11px] text-muted-foreground">{{ t('projectDetail.worktrees.setupCmdHint') }}</p>
    </div>
    <div v-if="worktrees.length" class="border-t border-border/60 pt-3 space-y-1.5">
      <div
        v-for="wt in worktrees"
        :key="wt.id"
        class="flex items-center gap-2 border border-border rounded-md px-3 py-2"
      >
        <div class="min-w-0 flex-1">
          <p class="text-xs font-medium font-mono truncate">{{ wt.branch }}</p>
          <p class="text-[11px] text-muted-foreground truncate">{{ t('projectDetail.worktrees.from', { base: wt.baseRef }) }}</p>
        </div>
        <Button
          variant="destructive-ghost"
          size="icon-sm"
          class="shrink-0"
          :title="t('projectDetail.worktrees.removeTitle')"
          @click="remove(wt)"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </div>
    </div>
  </Card>
</template>
