<script setup lang="ts">
// Rules — a list-detail screen like the Notes window: the sidebar lists the
// rules in their group folders (search, group select, Claude/Codex filter,
// Manage groups · Re-apply all · Import .md), the right pane edits the
// selected rule in place (components/rules/RuleDetail).
//
// The selection lives in the URL query (`?id=<rule>`, `?new=1` for a blank
// draft) so a reload or a link reopens it.
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Layers, RefreshCw, ScrollText, Upload } from 'lucide-vue-next'
import { AppSelect, Badge, Button } from '@/components/ui'
import GroupManagerModal from '@/components/GroupManagerModal.vue'
import ListDetailSidebar from '@/components/listDetail/ListDetailSidebar.vue'
import DetailEmpty from '@/components/listDetail/DetailEmpty.vue'
import RuleDetail from '@/components/rules/RuleDetail.vue'
import { useActivatedRefresh } from '@/composables/useActivatedRefresh'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useRulesStore, type Rule } from '@/stores/rules'
import { useProjectsStore } from '@/stores/projects'
import { useRuleGroupsStore, type Group } from '@/stores/groups'
import { UNGROUPED, buildRuleGroups } from '@/lib/rulesRows'

// Named explicitly: <KeepAlive :include> in App.vue matches on component name,
// and an inferred name is a build detail that minification can change.
defineOptions({ name: 'RulesView' })

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const store = useRulesStore()
const projectsStore = useProjectsStore()
const groupsStore = useRuleGroupsStore()
const { confirm } = useConfirm()
const { toast } = useToast()

const deletingId = ref<string | null>(null)
const applyingId = ref<string | null>(null)
/** Item id being re-applied, 'all' for the toolbar action, null when idle. */
const reapplyingId = ref<string | null>(null)
const importing = ref(false)
const groupsOpen = ref(false)

// ── Sidebar ─────────────────────────────────────────────────────────────────

const search = ref('')
/** '' = all groups, UNGROUPED, or a group id. */
const groupFilter = ref('')

/**
 * Rule bodies, so the search reaches the content too (the list only carries
 * name + description). Refetched per rule when its `updated_at` moves.
 */
const contents = ref<Record<string, string>>({})
const contentStamps = new Map<string, string>()

async function loadContents() {
  const stale = store.rules.filter((r) => contentStamps.get(r.id) !== r.updated_at)
  await Promise.all(stale.map(async (r) => {
    try {
      const rc = await store.getRule(r.id)
      contents.value = { ...contents.value, [r.id]: rc.content }
      contentStamps.set(r.id, r.updated_at)
    } catch {
      /* search falls back to name + description */
    }
  }))
}

watch(() => store.rules, () => void loadContents())

const sidebarGroups = computed(() =>
  buildRuleGroups(store.rules, {
    groups: groupsStore.groups,
    itemGroups: groupsStore.itemGroups,
    contents: contents.value,
    search: search.value,
    groupFilter: groupFilter.value,
    noneLabel: t('groups.ungrouped'),
  }),
)

const groupOptions = computed(() => [
  { value: '', label: t('rules.detail.allGroups') },
  ...groupsStore.groups.map((g) => ({ value: g.id, label: `${g.name} (${g.member_count})` })),
  { value: UNGROUPED, label: t('groups.ungrouped') },
])

const targetLabel = computed<Record<string, string>>(() => ({
  claude: t('rules.editor.targetClaude'),
  codex: t('rules.editor.targetCodex'),
  both: t('rules.editor.targetBoth'),
}))

const groupById = computed(() => new Map(groupsStore.groups.map((g) => [g.id, g])))

function groupsOf(ruleId: string): Group[] {
  const ids = groupsStore.itemGroups[ruleId] ?? []
  return groupsStore.groups.filter((g) => ids.includes(g.id))
}

// ── Selection (URL query) ───────────────────────────────────────────────────

const selectedId = ref<string | null>(null)
const isNew = ref(false)
const detailRef = ref<InstanceType<typeof RuleDetail> | null>(null)
/** Bumped by "+ New" so a second click starts over from a blank draft. */
const draftSeq = ref(0)
/** Remounts the detail per selection, so each one starts from a clean draft. */
const detailKey = computed(() => (isNew.value ? `new-${draftSeq.value}` : selectedId.value ?? ''))
const selectedRule = computed(() => store.rules.find((r) => r.id === selectedId.value) ?? null)
const rulesLoaded = ref(false)

function readQuery() {
  const id = typeof route.query.id === 'string' ? route.query.id : null
  return { id, isNew: !id && route.query.new === '1' }
}

// The view stays alive (KeepAlive) on other routes: only /rules' own query
// drives the selection, and coming back via a bare /rules restores it.
watch(
  () => [route.path, route.query.id, route.query.new],
  () => {
    if (route.path !== '/rules') return
    const q = readQuery()
    if (q.id || q.isNew) {
      if (q.id === selectedId.value && q.isNew === isNew.value) return
      void adoptQuery(q)
    } else if (selectedId.value || isNew.value) {
      void router.replace({ query: isNew.value ? { new: '1' } : { id: selectedId.value! } })
    }
  },
  { immediate: true },
)

/**
 * A selection arriving from outside (a link, an old editor URL's redirect):
 * our own changes set the local state first, so only these reach here.
 * Unsaved edits are not dropped silently — declining restores the URL.
 */
async function adoptQuery(q: { id: string | null; isNew: boolean }) {
  if (!(await confirmDiscard())) {
    void router.replace({ query: isNew.value ? { new: '1' } : selectedId.value ? { id: selectedId.value } : {} })
    return
  }
  selectedId.value = q.id
  isNew.value = q.isNew
}

function setSelection(id: string | null, draft = false) {
  selectedId.value = id
  isNew.value = draft
  void router.replace({ query: draft ? { new: '1' } : id ? { id } : {} })
}

/** Leaving unsaved edits takes a confirm; true = go ahead. */
async function confirmDiscard(): Promise<boolean> {
  if (!detailRef.value?.dirty) return true
  return confirm({
    title: t('rules.detail.confirmDiscardTitle'),
    message: t('rules.detail.confirmDiscardMessage'),
    confirmLabel: t('rules.detail.confirmDiscard'),
  })
}

async function select(id: string) {
  if (id === selectedId.value && !isNew.value) return
  if (!(await confirmDiscard())) return
  setSelection(id)
}

async function startNew() {
  if (isNew.value && !detailRef.value?.dirty) return
  if (!(await confirmDiscard())) return
  // A fresh draft even when one is already open (it was confirmed above).
  draftSeq.value++
  setSelection(null, true)
}

async function discardDraft() {
  if (!(await confirmDiscard())) return
  setSelection(null)
}

function onSaved(rule: Rule, created: boolean) {
  if (created) setSelection(rule.id)
}

// The selected rule vanished (deleted elsewhere, or a stale ?id=).
watch(
  () => [store.rules, rulesLoaded.value] as const,
  () => {
    if (rulesLoaded.value && selectedId.value && !store.error && !selectedRule.value) setSelection(null)
  },
)

// ── Data ────────────────────────────────────────────────────────────────────

async function refresh() {
  await Promise.all([store.fetchRules(), projectsStore.fetchProjects(), groupsStore.fetchGroups()])
  rulesLoaded.value = true
}

onMounted(() => void refresh())

// Cached by <KeepAlive> (App.vue): onMounted fires once for the app's whole
// lifetime, so re-show the screen with fresh data instead of whatever it read
// the very first time.
useActivatedRefresh(refresh)

// ── Actions ─────────────────────────────────────────────────────────────────

async function handleApplyToAll(rule: Rule) {
  const total = projectsStore.projects.length
  if (total === 0) {
    toast.error(t('rules.list.toastNoProjects'))
    return
  }
  if (!(await confirm({
    title: t('rules.list.confirmApplyTitle'),
    message: t('rules.list.confirmApplyMessage', { name: rule.name, count: total, project: t(total === 1 ? 'rules.list.project' : 'rules.list.projects') }),
    confirmLabel: t('rules.list.confirmApplyConfirm'),
    variant: 'primary',
  }))) return
  applyingId.value = rule.id
  try {
    const result = await store.applyRuleToAllProjects(rule.id)
    if (result.failures.length === 0) {
      toast.success(t('rules.list.toastApplied', { name: rule.name, count: result.applied, project: t(result.applied === 1 ? 'rules.list.project' : 'rules.list.projects') }))
    } else {
      const details = result.failures.map(f => `• ${f.project_name}: ${f.error}`).join('\n')
      toast.error(t('rules.list.toastAppliedPartial', { applied: result.applied, project: t(result.applied === 1 ? 'rules.list.project' : 'rules.list.projects'), failed: result.failures.length, details }))
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    applyingId.value = null
  }
}

/** Re-write one rule (or all of them) into the projects it is already applied to. */
async function handleReapply(rule?: Rule) {
  if (!(await confirm({
    title: rule ? t('rules.list.confirmReapplyTitle') : t('rules.list.confirmReapplyAllTitle'),
    message: rule ? t('rules.list.confirmReapplyMessage', { name: rule.name }) : t('rules.list.confirmReapplyAllMessage'),
    confirmLabel: t('rules.list.confirmReapplyConfirm'),
    variant: 'primary',
  }))) return
  reapplyingId.value = rule?.id ?? 'all'
  try {
    const result = await store.reapplyRules(rule?.id)
    if (result.applied === 0 && result.failures.length === 0) {
      toast.info(t('rules.list.toastNothingToReapply'))
    } else if (result.failures.length === 0) {
      toast.success(t('rules.list.toastReapplied', { count: result.applied }))
    } else {
      const details = result.failures.map(f => `• ${f.project_name}: ${f.error}`).join('\n')
      toast.error(t('rules.list.toastReappliedPartial', { applied: result.applied, failed: result.failures.length, details }))
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    reapplyingId.value = null
  }
}

async function handleDelete(rule: Rule) {
  if (!(await confirm({
    title: t('rules.list.confirmDeleteTitle'),
    message: t('rules.list.confirmDeleteMessage', { name: rule.name }),
    confirmLabel: t('common.delete'),
  }))) return
  deletingId.value = rule.id
  try {
    // Cleared first so the vanished-selection watcher has nothing to do.
    if (selectedId.value === rule.id) setSelection(null)
    await store.deleteRule(rule.id)
    toast.success(t('rules.list.toastDeleted'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    deletingId.value = null
  }
}

async function handleImport() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }],
  })
  if (!selected) return
  importing.value = true
  try {
    const rule = await store.importRule(selected as string)
    toast.success(t('rules.list.toastImported'))
    if (await confirmDiscard()) setSelection(rule.id)
  } catch (e) {
    toast.error(String(e))
  } finally {
    importing.value = false
  }
}

async function handleExport(rule: Rule) {
  const destPath = await save({
    defaultPath: `${rule.name}.md`,
    filters: [{ name: 'Markdown', extensions: ['md'] }],
  })
  if (!destPath) return
  try {
    await store.exportRule(rule.id, destPath)
    toast.success(t('rules.list.toastExported'))
  } catch (e) {
    toast.error(String(e))
  }
}
</script>

<template>
  <div class="flex h-full min-h-0">
    <ListDetailSidebar
      v-model:search="search"
      :groups="sidebarGroups"
      :selected-id="isNew ? null : selectedId"
      collapse-key="devdy.rules.collapsedGroups"
      :search-placeholder="t('rules.detail.searchPlaceholder')"
      :loading="store.loading"
      :error="store.error"
      :empty-store="store.rules.length === 0"
      :empty-title="t('rules.list.emptyTitle')"
      :empty-hint="t('rules.list.emptyHint')"
      @select="select"
      @add="startNew"
      @retry="refresh"
    >
      <template #toolbar>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :title="t('groups.manageRuleGroups')"
          :aria-label="t('groups.manageRuleGroups')"
          @click="groupsOpen = true"
        >
          <Layers class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :disabled="reapplyingId !== null || store.rules.length === 0"
          :title="reapplyingId === 'all' ? t('rules.list.reapplying') : `${t('rules.list.reapplyAll')} — ${t('rules.list.reapplyAllHint')}`"
          :aria-label="t('rules.list.reapplyAll')"
          @click="handleReapply()"
        >
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-spin': reapplyingId === 'all' }" />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :disabled="importing"
          :title="importing ? t('rules.list.importing') : t('rules.list.importMd')"
          :aria-label="t('rules.list.importMd')"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-pulse': importing }" />
        </Button>
      </template>

      <template #controls>
        <AppSelect
          v-if="groupsStore.groups.length > 0"
          v-model="groupFilter"
          size="sm"
          :options="groupOptions"
          :title="t('groups.filterByGroup')"
        />
      </template>

      <template #row="{ row, selected }">
        <span class="flex min-w-0 items-center gap-1.5">
          <span class="min-w-0 flex-1 truncate font-mono text-sm font-medium leading-snug" :class="selected && 'text-foreground'">
            {{ row.rule.name }}
          </span>
          <Badge v-if="row.rule.target !== 'both'" tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">
            {{ targetLabel[row.rule.target] }}
          </Badge>
        </span>
        <span class="mt-1 truncate text-[11px] text-muted-foreground/70">{{ row.rule.description }}</span>
        <span v-if="row.otherGroupIds.length > 0" class="mt-1.5 flex flex-wrap gap-1">
          <template v-for="gid in row.otherGroupIds" :key="gid">
            <Badge v-if="groupById.get(gid)" :tone="groupById.get(gid)!.color ?? 'neutral'" size="xs">
              {{ groupById.get(gid)!.name }}
            </Badge>
          </template>
        </span>
      </template>
    </ListDetailSidebar>

    <RuleDetail
      v-if="isNew || selectedRule"
      ref="detailRef"
      :key="detailKey"
      :rule-id="isNew ? null : selectedId"
      :rule="isNew ? null : selectedRule"
      :groups="isNew || !selectedId ? [] : groupsOf(selectedId)"
      :reapplying="!!selectedId && reapplyingId === selectedId"
      :reapply-busy="reapplyingId !== null"
      :applying="!!selectedId && applyingId === selectedId"
      :deleting="!!selectedId && deletingId === selectedId"
      @saved="onSaved"
      @reapply="selectedRule && handleReapply(selectedRule)"
      @apply-all="selectedRule && handleApplyToAll(selectedRule)"
      @export="selectedRule && handleExport(selectedRule)"
      @delete="selectedRule && handleDelete(selectedRule)"
      @discard="discardDraft"
      @missing="setSelection(null)"
    />
    <DetailEmpty v-else :icon="ScrollText" :text="t('rules.detail.noSelection')" />

    <GroupManagerModal
      :open="groupsOpen"
      kind="rule"
      :items="store.rules"
      @close="groupsOpen = false"
      @changed="groupsStore.fetchGroups()"
    />
  </div>
</template>
