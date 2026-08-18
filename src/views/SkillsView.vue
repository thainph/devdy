<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useSkillsStore, type Skill } from '@/stores/skills'
import { useProjectsStore } from '@/stores/projects'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Button, Card, Badge } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { Plus, Upload, Download, Pencil, Trash2, Puzzle, CalendarDays, FolderCheck } from 'lucide-vue-next'

const { t } = useI18n()
const router = useRouter()
const store = useSkillsStore()
const projectsStore = useProjectsStore()
const { confirm } = useConfirm()
const { toast } = useToast()
const deletingId = ref<string | null>(null)
const applyingId = ref<string | null>(null)
const importingZip = ref(false)
const targetLabel: Record<string, string> = { claude: t('skills.editor.targetClaude'), codex: t('skills.editor.targetCodex'), both: t('skills.editor.targetBoth') }

onMounted(() => {
  store.fetchSkills()
  projectsStore.fetchProjects()
})

async function handleApplyToAll(skill: Skill) {
  const total = projectsStore.projects.length
  if (total === 0) {
    toast.error(t('skills.list.toastNoProjects'))
    return
  }
  if (!(await confirm({
    title: t('skills.list.confirmApplyTitle'),
    message: t('skills.list.confirmApplyMessage', { name: skill.name, count: total, project: t(total === 1 ? 'skills.list.project' : 'skills.list.projects') }),
    confirmLabel: t('skills.list.confirmApplyConfirm'),
    variant: 'primary',
  }))) return
  applyingId.value = skill.id
  try {
    const result = await store.applySkillToAllProjects(skill.id)
    if (result.failures.length === 0) {
      toast.success(t('skills.list.toastApplied', { name: skill.name, count: result.applied, project: t(result.applied === 1 ? 'skills.list.project' : 'skills.list.projects') }))
    } else {
      const details = result.failures.map(f => `• ${f.project_name}: ${f.error}`).join('\n')
      toast.error(t('skills.list.toastAppliedPartial', { applied: result.applied, project: t(result.applied === 1 ? 'skills.list.project' : 'skills.list.projects'), failed: result.failures.length, details }))
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    applyingId.value = null
  }
}

async function handleDelete(skill: Skill) {
  if (!(await confirm({
    title: t('skills.list.confirmDeleteTitle'),
    message: t('skills.list.confirmDeleteMessage', { name: skill.name }),
    confirmLabel: t('common.delete'),
  }))) return
  deletingId.value = skill.id
  try {
    await store.deleteSkill(skill.id)
    toast.success(t('skills.list.toastDeleted'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    deletingId.value = null
  }
}

async function handleImport() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'Skill', extensions: ['zip', 'skill'] }],
  })
  if (!selected) return
  importingZip.value = true
  try {
    await store.importSkillZip(selected)
    toast.success(t('skills.list.toastImported'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    importingZip.value = false
  }
}

async function handleExport(skill: Skill) {
  const destPath = await save({
    defaultPath: `${skill.name}.zip`,
    filters: [{ name: 'Zip', extensions: ['zip'] }],
  })
  if (!destPath) return
  try {
    await store.exportSkillZip(skill.id, destPath)
    toast.success(t('skills.list.toastExported'))
  } catch (e) {
    toast.error(String(e))
  }
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">{{ t('skills.list.title') }}</h1>
        <span
          v-if="!store.loading && store.skills.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.skills.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button
          variant="outline"
          :disabled="importingZip"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ importingZip ? t('skills.list.importing') : t('skills.list.import') }}
        </Button>
        <Button
          @click="router.push('/skills/new')"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('skills.list.newSkill') }}
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Loading skeleton -->
      <div v-if="store.loading" class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <div v-for="i in 5" :key="i" class="h-27 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="store.skills.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <Puzzle class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('skills.list.emptyTitle') }}</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-50">{{ t('skills.list.emptyHint') }}</p>
        <Button
          class="mt-4"
          @click="router.push('/skills/new')"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('skills.list.newSkill') }}
        </Button>
      </div>

      <!-- Cards grid -->
      <div v-else class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <Card
          v-for="skill in store.skills"
          :key="skill.id"
          class="group relative flex flex-col transition-all duration-150 cursor-pointer hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)] hover:-translate-y-0.5"
          body-class="flex flex-col flex-1 p-4"
          @click="router.push(`/skills/${skill.id}/edit`)"
        >
          <!-- hover accent line -->
          <span class="absolute inset-x-0 top-0 h-0.5 origin-left scale-x-0 bg-linear-to-r from-primary to-primary/30 transition-transform duration-200 group-hover:scale-x-100" />

          <!-- Icon + name/desc -->
          <div class="flex items-start gap-3 mb-3">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary transition-colors group-hover:bg-primary/15 group-hover:border-primary/25">
              <Puzzle class="h-4.5 w-4.5" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5">
                <p class="text-sm font-semibold font-mono truncate leading-tight">{{ skill.name }}</p>
              </div>
              <p class="text-xs text-muted-foreground mt-1 line-clamp-2 leading-relaxed">{{ skill.description }}</p>
            </div>
          </div>

          <!-- Footer -->
          <div class="flex items-center justify-between pt-2.5 border-t border-border/60 mt-auto">
            <div class="flex items-center gap-2 min-w-0">
              <Badge tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">
                {{ targetLabel[skill.target] }}
              </Badge>
              <div class="flex items-center gap-1 text-[10px] text-muted-foreground/60">
                <CalendarDays class="h-3 w-3" :stroke-width="1.5" />
                <span>{{ formatDate(skill.updated_at) }}</span>
              </div>
            </div>
            <!-- Actions on hover -->
            <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity" @click.stop>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-default"
                :title="t('skills.list.applyToAll')"
                :disabled="applyingId === skill.id"
                @click="handleApplyToAll(skill)"
              >
                <FolderCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                :title="t('skills.list.exportZip')"
                @click="handleExport(skill)"
              >
                <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                :title="t('common.edit')"
                @click="router.push(`/skills/${skill.id}/edit`)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-destructive/60 hover:text-destructive hover:bg-destructive/10 transition-colors cursor-pointer disabled:opacity-40"
                :title="t('common.delete')"
                :disabled="deletingId === skill.id"
                @click="handleDelete(skill)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  </div>
</template>
