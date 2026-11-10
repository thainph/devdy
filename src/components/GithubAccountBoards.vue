<script setup lang="ts">
// GitHub Project V2 boards of one GitHub account (Settings → GitHub). A board is
// configured once here (URL + Start / Deadline / Status mapping) and then only
// switched on in each project's "GitHub Project" tab.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { KanbanSquare, Loader2, Pencil, Plus, Trash2 } from 'lucide-vue-next'
import { AppSelect, Banner, Button, Input } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import {
  parseBoardMapping, useGithubBoardsStore, type BoardInfo, type GithubBoard,
} from '@/stores/githubBoards'
import { useProjectsStore } from '@/stores/projects'

const props = defineProps<{ accountId: string }>()

const { t } = useI18n()
const { confirm } = useConfirm()
const { toast } = useToast()
const boardsStore = useGithubBoardsStore()
const projectStore = useProjectsStore()

const boards = computed(() => boardsStore.boards.filter(b => b.account_id === props.accountId))

function usedBy(board: GithubBoard): number {
  return projectStore.projects.filter(p => p.github_board_id === board.id).length
}

function mappingSummary(board: GithubBoard): string {
  const m = parseBoardMapping(board.field_mappings)
  const mapped = [
    m.startFieldId && t('settings.githubBoards.start'),
    m.deadlineFieldId && t('settings.githubBoards.deadline'),
    m.statusFieldId && t('settings.githubBoards.status'),
  ].filter(Boolean)
  return mapped.length ? mapped.join(' · ') : t('settings.githubBoards.noFieldsMapped')
}

// --- Add / edit form (one at a time) ---
/** null = closed, 'new' = adding, else the id of the board being edited. */
const formFor = ref<string | null>(null)
const formUrl = ref('')
const info = ref<BoardInfo | null>(null)
const mapStart = ref('')
const mapDeadline = ref('')
const mapStatus = ref('')
const resolving = ref(false)
const saving = ref(false)
const formError = ref<string | null>(null)

const noneOpt = computed(() => ({ value: '', label: t('settings.githubBoards.none') }))
const dateFieldOptions = computed(() => [
  noneOpt.value,
  ...(info.value?.fields ?? [])
    .filter(f => f.dataType === 'DATE' || f.dataType === 'ITERATION')
    .map(f => ({ value: f.id, label: `${f.name} (${f.dataType})` })),
])
const statusFieldOptions = computed(() => [
  noneOpt.value,
  ...(info.value?.fields ?? [])
    .filter(f => f.dataType === 'SINGLE_SELECT')
    .map(f => ({ value: f.id, label: f.name })),
])

function resetForm() {
  formFor.value = null
  formUrl.value = ''
  info.value = null
  mapStart.value = ''
  mapDeadline.value = ''
  mapStatus.value = ''
  formError.value = null
}

function startAdd() {
  resetForm()
  formFor.value = 'new'
}

async function startEdit(board: GithubBoard) {
  resetForm()
  formFor.value = board.id
  formUrl.value = board.url
  const m = parseBoardMapping(board.field_mappings)
  await resolve()
  // Keep the saved mapping (resolve() leaves the selects untouched).
  mapStart.value = m.startFieldId ?? ''
  mapDeadline.value = m.deadlineFieldId ?? ''
  mapStatus.value = m.statusFieldId ?? ''
}

async function resolve() {
  if (!formUrl.value.trim()) return
  resolving.value = true
  formError.value = null
  try {
    info.value = await boardsStore.resolve(props.accountId, formUrl.value.trim())
  } catch (e) {
    info.value = null
    formError.value = String(e)
  } finally {
    resolving.value = false
  }
}

async function save() {
  if (!info.value) return
  saving.value = true
  formError.value = null
  try {
    // Editing keeps the stored URL so the same board row is updated.
    const editing = boards.value.find(b => b.id === formFor.value)
    await boardsStore.save(props.accountId, editing?.url ?? info.value.url, info.value, {
      startFieldId: mapStart.value || null,
      deadlineFieldId: mapDeadline.value || null,
      statusFieldId: mapStatus.value || null,
    })
    await projectStore.fetchProjects()
    toast.success(t('settings.githubBoards.saved'))
    resetForm()
  } catch (e) {
    formError.value = String(e)
  } finally {
    saving.value = false
  }
}

async function remove(board: GithubBoard) {
  const count = usedBy(board)
  if (!(await confirm({
    title: t('settings.githubBoards.deleteTitle'),
    message: count
      ? t('settings.githubBoards.deleteMessageUsed', { count })
      : t('settings.githubBoards.deleteMessage'),
    confirmLabel: t('common.delete'),
  }))) return
  try {
    await boardsStore.remove(board.id)
    await projectStore.fetchProjects()
    toast.success(t('settings.githubBoards.deleted'))
  } catch (e) {
    toast.error(String(e))
  }
}
</script>

<template>
  <div class="border-t border-border/60 pt-2 space-y-2">
    <div class="flex items-center gap-1.5">
      <KanbanSquare class="h-3 w-3 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-[11px] font-medium text-muted-foreground">{{ t('settings.githubBoards.title') }}</span>
      <Button
        v-if="formFor === null"
        variant="ghost"
        size="xs"
        class="ml-auto"
        @click="startAdd"
      >
        <Plus class="h-3 w-3" :stroke-width="2" />
        {{ t('settings.githubBoards.add') }}
      </Button>
    </div>

    <p v-if="!boards.length && formFor === null" class="text-[11px] text-muted-foreground">
      {{ t('settings.githubBoards.empty') }}
    </p>

    <div
      v-for="board in boards"
      v-show="formFor !== board.id"
      :key="board.id"
      class="flex items-center gap-2 rounded-md bg-muted/40 px-2.5 py-1.5"
    >
      <div class="min-w-0 flex-1">
        <div class="text-xs font-medium truncate">{{ board.title || board.url }}</div>
        <div class="text-[10px] text-muted-foreground truncate">
          <span v-if="board.title">{{ board.url }} · </span>{{ mappingSummary(board) }}
          · {{ t('settings.githubBoards.usedBy', usedBy(board)) }}
        </div>
      </div>
      <Button
        variant="ghost"
        size="icon-sm"
        :title="t('common.edit')"
        :aria-label="t('common.edit')"
        :disabled="formFor !== null"
        @click="startEdit(board)"
      >
        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        variant="destructive-ghost"
        size="icon-sm"
        :title="t('common.delete')"
        :aria-label="t('common.delete')"
        @click="remove(board)"
      >
        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </div>

    <!-- Add / edit form -->
    <div v-if="formFor !== null" class="rounded-md border border-border p-2.5 space-y-2.5">
      <div class="space-y-1.5">
        <label class="text-[11px] font-medium text-muted-foreground">{{ t('settings.githubBoards.url') }}</label>
        <div class="flex gap-2">
          <Input
            v-model="formUrl"
            size="sm"
            placeholder="https://github.com/orgs/<owner>/projects/<n>"
            class="flex-1"
            :disabled="resolving || saving || formFor !== 'new'"
            @keyup.enter="resolve"
          />
          <Button
            v-if="formFor === 'new'"
            variant="outline"
            size="sm"
            :disabled="resolving || !formUrl.trim()"
            @click="resolve"
          >
            <Loader2 v-if="resolving" class="h-3.5 w-3.5 animate-spin" />
            <span>{{ resolving ? t('settings.githubBoards.connecting') : t('settings.githubBoards.connect') }}</span>
          </Button>
        </div>
        <p class="text-[10px] text-muted-foreground">
          {{ t('settings.githubBoards.scopeHint') }} <code class="text-primary">read:project</code>
        </p>
      </div>

      <Banner v-if="formError" tone="destructive">{{ formError }}</Banner>
      <div v-else-if="resolving && formFor !== 'new'" class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
        <Loader2 class="h-3 w-3 animate-spin" /> {{ t('settings.githubBoards.connecting') }}
      </div>

      <template v-if="info">
        <div class="text-[11px] text-muted-foreground">
          {{ t('settings.githubBoards.boardLabel') }}
          <span class="text-foreground font-medium">{{ info.title }}</span>
          · {{ t('settings.githubBoards.fieldsCount', { count: info.fields.length }) }}
        </div>
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium text-muted-foreground">{{ t('settings.githubBoards.startField') }}</label>
          <AppSelect v-model="mapStart" size="sm" :options="dateFieldOptions" />
        </div>
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium text-muted-foreground">{{ t('settings.githubBoards.deadlineField') }}</label>
          <AppSelect v-model="mapDeadline" size="sm" :options="dateFieldOptions" />
        </div>
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium text-muted-foreground">{{ t('settings.githubBoards.statusField') }}</label>
          <AppSelect v-model="mapStatus" size="sm" :options="statusFieldOptions" />
        </div>
      </template>

      <div class="flex gap-2">
        <Button size="sm" :disabled="!info || saving" @click="save">
          <Loader2 v-if="saving" class="h-3.5 w-3.5 animate-spin" />
          <span>{{ t('common.save') }}</span>
        </Button>
        <Button variant="outline" size="sm" :disabled="saving" @click="resetForm">{{ t('common.cancel') }}</Button>
      </div>
    </div>
  </div>
</template>
