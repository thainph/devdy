<script setup lang="ts">
// Skills as a list-detail screen (the Notes window's layout): the sidebar
// lists the skills in their groups, the right pane edits the selected one.
// The selection lives in the URL — `?id=<id>` or `?new=1` for a draft.
import { computed, onDeactivated, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { onBeforeRouteUpdate, useRoute, useRouter, type LocationQuery } from 'vue-router'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Layers, Plus, Puzzle, RefreshCw, Upload } from 'lucide-vue-next'
import { useActivatedRefresh } from '@/composables/useActivatedRefresh'
import { useSkillsStore, type Skill } from '@/stores/skills'
import { useProjectsStore } from '@/stores/projects'
import { useSkillGroupsStore } from '@/stores/groups'
import { AppSelect, Badge, Button } from '@/components/ui'
import GroupManagerModal from '@/components/GroupManagerModal.vue'
import ListDetailSidebar from '@/components/listDetail/ListDetailSidebar.vue'
import DetailEmpty from '@/components/listDetail/DetailEmpty.vue'
import SkillDetail from '@/components/skills/SkillDetail.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { FILTER_UNGROUPED, buildSkillGroups } from '@/lib/skillRows'

// Named explicitly: <KeepAlive :include> in App.vue matches on component name,
// and an inferred name is a build detail that minification can change.
defineOptions({ name: 'SkillsView' })

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const store = useSkillsStore()
const projectsStore = useProjectsStore()
const groupsStore = useSkillGroupsStore()
const { confirm } = useConfirm()
const { toast } = useToast()
const deletingId = ref<string | null>(null)
const applyingId = ref<string | null>(null)
/** Item id being re-applied, 'all' for the header action, null when idle. */
const reapplyingId = ref<string | null>(null)
const importingZip = ref(false)
const groupsOpen = ref(false)
const search = ref('')
/** '' = all groups, a group id, or FILTER_UNGROUPED. */
const groupFilter = ref('')
const targetLabel = computed<Record<string, string>>(() => ({
  claude: t('skills.editor.targetClaude'),
  codex: t('skills.editor.targetCodex'),
  both: t('skills.editor.targetBoth'),
}))

const rowGroups = computed(() =>
  buildSkillGroups(store.skills, groupsStore.itemGroups, groupsStore.groups, {
    search: search.value,
    group: groupFilter.value,
    ungroupedLabel: t('groups.ungrouped'),
  }),
)

const groupOptions = computed(() => [
  { value: '', label: t('groups.allGroups') },
  ...groupsStore.groups.map((g) => ({ value: g.id, label: `${g.name} (${g.member_count})` })),
  { value: FILTER_UNGROUPED, label: t('groups.ungrouped') },
])

// A filtered-out group was deleted in the group manager.
watch(() => groupsStore.groups, (list) => {
  if (groupFilter.value && groupFilter.value !== FILTER_UNGROUPED && !list.some((g) => g.id === groupFilter.value)) {
    groupFilter.value = ''
  }
})

function groupsOf(skillId: string) {
  const ids = groupsStore.itemGroups[skillId] ?? []
  return groupsStore.groups.filter((g) => ids.includes(g.id))
}

function groupById(id: string) {
  return groupsStore.groups.find((g) => g.id === id)
}

// ── Selection (URL) ─────────────────────────────────────────────────────────

interface Selection {
  id: string | null
  isNew: boolean
}

function fromQuery(q: LocationQuery): Selection {
  const id = typeof q.id === 'string' && q.id ? q.id : null
  return { id, isNew: !id && q.new === '1' }
}

function sameSelection(a: Selection, b: Selection): boolean {
  return a.id === b.id && a.isNew === b.isNew
}

const selection = ref<Selection>(fromQuery(route.query))
const selectedId = computed(() => selection.value.id)
const isNew = computed(() => selection.value.isNew)
const selectedSkill = computed(() => store.skills.find((s) => s.id === selectedId.value) ?? null)
/** Remounts the detail per selection: fresh editor, fresh dirty baseline. */
const detailKey = computed(() => selectedId.value ?? (isNew.value ? 'new' : null))

function toQuery(s: Selection): Record<string, string> {
  return s.id ? { id: s.id } : s.isNew ? { new: '1' } : {}
}

// The nav link opens a bare /skills. Coming back that way restores the URL of
// what the cached screen still shows instead of dropping it (and its unsaved
// edits). Entering with another selection (a link from elsewhere) bypasses
// onBeforeRouteUpdate, so unsaved edits are confirmed here and a "no" puts the
// URL back. Within the screen the URL is followed as is.
let prevRouteName = route.name
let enterSeq = 0
watch(() => route.fullPath, async () => {
  const entering = prevRouteName !== route.name
  prevRouteName = route.name
  if (route.name !== 'skills') return
  const next = fromQuery(route.query)
  const showing = selection.value
  const seq = ++enterSeq
  if (entering && !sameSelection(next, showing) && (showing.id || showing.isNew)) {
    if (!next.id && !next.isNew) {
      void router.replace({ query: toQuery(showing) })
      return
    }
    if (!(await confirmDiscard())) {
      if (seq === enterSeq) void router.replace({ query: toQuery(showing) })
      return
    }
    if (seq !== enterSeq) return
  }
  selection.value = next
})
// In case a cached view's watchers are paused while it is hidden.
onDeactivated(() => {
  prevRouteName = undefined
})
const detailRef = ref<InstanceType<typeof SkillDetail> | null>(null)
/** >0 while a replace must not ask about unsaved changes (they were just deleted). */
let skipDiscardCheck = 0

async function confirmDiscard(): Promise<boolean> {
  if (skipDiscardCheck > 0 || !detailRef.value?.dirty) return true
  return confirm({
    title: t('skills.detail.discardTitle'),
    message: t('skills.detail.discardMessage'),
    confirmLabel: t('skills.detail.discard'),
  })
}

// Every selection change goes through the URL, so this one guard covers row
// clicks, "+ New", and back/forward alike.
onBeforeRouteUpdate(async (to) => {
  if (!sameSelection(fromQuery(to.query), selection.value) && !(await confirmDiscard())) return false
})

function select(id: string | null) {
  void router.replace({ query: id ? { id } : {} })
}

function openNew() {
  void router.replace({ query: { new: '1' } })
}

async function replaceWithoutCheck(query: Record<string, string>) {
  skipDiscardCheck++
  try {
    await router.replace({ query })
  } finally {
    skipDiscardCheck--
  }
}

function onSaved(skill: Skill, created: boolean) {
  if (created) void replaceWithoutCheck({ id: skill.id })
}

// The selected skill vanished (deleted elsewhere, e.g. by the AI).
let loadedOnce = false
watch(() => store.loading, (now, before) => {
  if (before && !now && !store.error) loadedOnce = true
})
watch(() => store.skills, (list) => {
  if (loadedOnce && selectedId.value && !list.some((s) => s.id === selectedId.value)) {
    void replaceWithoutCheck({})
  }
})

// ── Data ────────────────────────────────────────────────────────────────────

function refresh() {
  store.fetchSkills()
  projectsStore.fetchProjects()
  groupsStore.fetchGroups()
}

onMounted(refresh)

// Cached by <KeepAlive> (App.vue): onMounted fires once for the app's whole
// lifetime, so re-show the screen with fresh data instead of whatever it read
// the very first time.
useActivatedRefresh(refresh)

// ── Actions ─────────────────────────────────────────────────────────────────

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

/** Re-write one skill (or all of them) into the projects it is already applied to. */
async function handleReapply(skill?: Skill) {
  if (!(await confirm({
    title: skill ? t('skills.list.confirmReapplyTitle') : t('skills.list.confirmReapplyAllTitle'),
    message: skill ? t('skills.list.confirmReapplyMessage', { name: skill.name }) : t('skills.list.confirmReapplyAllMessage'),
    confirmLabel: t('skills.list.confirmReapplyConfirm'),
    variant: 'primary',
  }))) return
  reapplyingId.value = skill?.id ?? 'all'
  try {
    const result = await store.reapplySkills(skill?.id)
    if (result.applied === 0 && result.failures.length === 0) {
      toast.info(t('skills.list.toastNothingToReapply'))
    } else if (result.failures.length === 0) {
      toast.success(t('skills.list.toastReapplied', { count: result.applied }))
    } else {
      const details = result.failures.map(f => `• ${f.project_name}: ${f.error}`).join('\n')
      toast.error(t('skills.list.toastReappliedPartial', { applied: result.applied, failed: result.failures.length, details }))
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    reapplyingId.value = null
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
    // Its unsaved edits (if any) go with it — the delete was confirmed.
    if (selectedId.value === skill.id) await replaceWithoutCheck({})
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
    const skill = await store.importSkillZip(selected)
    toast.success(t('skills.list.toastImported'))
    select(skill.id)
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

</script>

<template>
  <div class="flex h-full overflow-hidden">
    <ListDetailSidebar
      v-model:search="search"
      :groups="rowGroups"
      :selected-id="selectedId"
      collapse-key="devdy.skills.collapsed"
      :search-placeholder="t('skills.detail.searchPlaceholder')"
      :loading="store.loading"
      :error="store.error"
      :empty-store="!store.loading && store.skills.length === 0"
      :empty-title="t('skills.list.emptyTitle')"
      :empty-hint="t('skills.list.emptyHint')"
      @select="select"
      @add="openNew"
      @retry="refresh"
    >
      <template #toolbar>
        <Button
          variant="ghost"
          size="icon-sm"
          :title="t('groups.manageSkillGroups')"
          :aria-label="t('groups.manageSkillGroups')"
          @click="groupsOpen = true"
        >
          <Layers class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          v-if="store.skills.length > 0"
          variant="ghost"
          size="icon-sm"
          :disabled="reapplyingId !== null"
          :title="reapplyingId === 'all' ? t('skills.list.reapplying') : `${t('skills.list.reapplyAll')} — ${t('skills.list.reapplyAllHint')}`"
          :aria-label="t('skills.list.reapplyAll')"
          @click="handleReapply()"
        >
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-spin': reapplyingId === 'all' }" />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          :disabled="importingZip"
          :title="importingZip ? t('skills.list.importing') : t('skills.detail.importHint')"
          :aria-label="t('skills.list.import')"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-pulse': importingZip }" />
        </Button>
      </template>

      <template #controls>
        <AppSelect v-if="groupsStore.groups.length > 0" v-model="groupFilter" :options="groupOptions" />
      </template>

      <template #row="{ row }">
        <span class="flex min-w-0 items-center gap-1.5">
          <span class="min-w-0 truncate font-mono text-sm font-medium leading-snug">{{ row.skill.name }}</span>
          <Badge v-if="row.skill.target !== 'both'" tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">{{ targetLabel[row.skill.target] }}</Badge>
          <template v-for="gid in row.otherGroupIds" :key="gid">
            <Badge v-if="groupById(gid)" :tone="groupById(gid)?.color ?? 'neutral'" size="xs" class="shrink-0">
              {{ groupById(gid)?.name }}
            </Badge>
          </template>
        </span>
        <span v-if="row.skill.description" class="mt-1 truncate text-[11px] text-muted-foreground/70">
          {{ row.skill.description }}
        </span>
      </template>
    </ListDetailSidebar>

    <SkillDetail
      v-if="detailKey"
      ref="detailRef"
      :key="detailKey"
      :skill-id="selectedId"
      :skill="selectedSkill"
      :groups="selectedId ? groupsOf(selectedId) : []"
      :applying="applyingId === selectedId"
      :reapply-busy="reapplyingId !== null"
      :reapplying="reapplyingId === selectedId"
      :deleting="deletingId === selectedId"
      @saved="onSaved"
      @missing="replaceWithoutCheck({})"
      @apply-all="selectedSkill && handleApplyToAll(selectedSkill)"
      @reapply="selectedSkill && handleReapply(selectedSkill)"
      @export="selectedSkill && handleExport(selectedSkill)"
      @delete="selectedSkill && handleDelete(selectedSkill)"
    />
    <DetailEmpty
      v-else
      :icon="Puzzle"
      :text="store.skills.length ? t('skills.detail.noSelection') : t('skills.list.emptyHint')"
    >
      <div class="mt-4 flex items-center justify-center gap-2">
        <Button variant="outline" size="sm" :disabled="importingZip" @click="handleImport">
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ importingZip ? t('skills.list.importing') : t('skills.list.import') }}
        </Button>
        <Button size="sm" @click="openNew">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('common.new') }}
        </Button>
      </div>
    </DetailEmpty>

    <GroupManagerModal
      :open="groupsOpen"
      kind="skill"
      :items="store.skills"
      @close="groupsOpen = false"
      @changed="groupsStore.fetchGroups()"
    />
  </div>
</template>
