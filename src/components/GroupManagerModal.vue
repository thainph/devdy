<script setup lang="ts">
// Group manager for skills and rules: create a group, pick its members, and (optionally) push it
// to every project at once. One component serves both kinds — see `stores/groups.ts`.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Badge, Input, Modal, Textarea } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useProjectsStore } from '@/stores/projects'
import { GROUP_COLORS, useGroupsStore, type Group, type GroupColor, type GroupKind } from '@/stores/groups'
import { Check, FolderCheck, Layers, Loader2, Plus, Trash2 } from 'lucide-vue-next'

const props = defineProps<{
  open: boolean
  kind: GroupKind
  /** Every skill (or rule) available to be put in a group. */
  items: { id: string; name: string; description: string }[]
}>()

const emit = defineEmits<{ close: []; changed: [] }>()

const { t } = useI18n()
const { confirm } = useConfirm()
const { toast } = useToast()
const store = useGroupsStore(props.kind)
const projectsStore = useProjectsStore()

const selectedId = ref<string | null>(null)
const draftName = ref('')
const draftDescription = ref('')
const draftColor = ref<GroupColor | null>(null)
const memberIds = ref<string[]>([])
const creatingName = ref('')
const creating = ref(false)
const saving = ref(false)
const applyingAll = ref(false)

const selectedGroup = computed(() => store.groups.find(g => g.id === selectedId.value) ?? null)

watch(
  () => props.open,
  async (open) => {
    if (!open) return
    await store.fetchGroups()
    if (!selectedId.value || !store.groups.some(g => g.id === selectedId.value)) {
      selectGroup(store.groups[0] ?? null)
    }
  },
  { immediate: true },
)

async function selectGroup(group: Group | null) {
  selectedId.value = group?.id ?? null
  draftName.value = group?.name ?? ''
  draftDescription.value = group?.description ?? ''
  draftColor.value = group?.color ?? null
  memberIds.value = group ? await store.getMembers(group.id) : []
}

function toggleMember(itemId: string) {
  const index = memberIds.value.indexOf(itemId)
  if (index === -1) memberIds.value.push(itemId)
  else memberIds.value.splice(index, 1)
}

async function handleCreate() {
  const name = creatingName.value.trim()
  if (!name) return
  creating.value = true
  try {
    const group = await store.createGroup({ name, description: '', color: null })
    creatingName.value = ''
    await selectGroup(group)
    emit('changed')
  } catch (e) {
    toast.error(String(e))
  } finally {
    creating.value = false
  }
}

// Saving writes both the group fields and its membership; the backend then propagates the
// membership diff to every project already running this group.
async function handleSave() {
  const group = selectedGroup.value
  if (!group) return
  saving.value = true
  try {
    await store.updateGroup({
      id: group.id,
      name: draftName.value.trim(),
      description: draftDescription.value,
      color: draftColor.value,
    })
    const outcome = await store.setMembers(group.id, memberIds.value)
    if (outcome.failures.length > 0) {
      toast.error(outcome.failures.map(f => `• ${f.item_name}: ${f.error}`).join('\n'))
    } else if (outcome.applied > 0) {
      toast.success(t('groups.toastSavedPropagated', { count: outcome.applied }))
    } else {
      toast.success(t('groups.toastSaved'))
    }
    emit('changed')
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

async function handleDelete() {
  const group = selectedGroup.value
  if (!group) return
  if (!(await confirm({
    title: t('groups.confirmDeleteTitle'),
    message: t('groups.confirmDeleteMessage', { name: group.name }),
    confirmLabel: t('common.delete'),
  }))) return
  try {
    await store.deleteGroup(group.id)
    await selectGroup(store.groups[0] ?? null)
    toast.success(t('groups.toastDeleted'))
    emit('changed')
  } catch (e) {
    toast.error(String(e))
  }
}

async function handleApplyToAll() {
  const group = selectedGroup.value
  if (!group) return
  const total = projectsStore.projects.length
  if (total === 0) {
    toast.error(t('groups.toastNoProjects'))
    return
  }
  if (!(await confirm({
    title: t('groups.confirmApplyAllTitle'),
    message: t('groups.confirmApplyAllMessage', { name: group.name, count: total }),
    confirmLabel: t('groups.confirmApplyAllConfirm'),
    variant: 'primary',
  }))) return
  applyingAll.value = true
  try {
    const result = await store.applyGroupToAllProjects(group.id)
    if (result.failures.length === 0) {
      toast.success(t('groups.toastAppliedAll', { name: group.name, count: result.applied }))
    } else {
      const details = result.failures.map(f => `• ${f.project_name}: ${f.error}`).join('\n')
      toast.error(t('groups.toastAppliedAllPartial', { applied: result.applied, failed: result.failures.length, details }))
    }
    emit('changed')
  } catch (e) {
    toast.error(String(e))
  } finally {
    applyingAll.value = false
  }
}

const title = computed(() => t(props.kind === 'skill' ? 'groups.manageSkillGroups' : 'groups.manageRuleGroups'))
const membersLabel = computed(() => t(props.kind === 'skill' ? 'groups.membersSkill' : 'groups.membersRule'))
const emptyItemsLabel = computed(() => t(props.kind === 'skill' ? 'groups.noItemsSkill' : 'groups.noItemsRule'))
</script>

<template>
  <!-- Fixed-height body with two independently scrolling columns: the group list must stay put
       while a long member list scrolls, and `minmax(0,…)` keeps long names from widening the grid. -->
  <Modal :open="open" size="xl" :title="title" @close="emit('close')">
    <div class="grid grid-cols-[13rem_minmax(0,1fr)] h-[60vh] overflow-hidden">
      <!-- Group list -->
      <div class="border-r border-border/60 flex flex-col min-h-0 min-w-0">
        <div class="p-2 border-b border-border/60 flex items-center gap-1.5 shrink-0">
          <Input
            v-model="creatingName"
            :placeholder="t('groups.groupNamePlaceholder')"
            class="flex-1 min-w-0"
            @keydown.enter="handleCreate"
          />
          <Button variant="outline" size="icon-sm" :disabled="creating || !creatingName.trim()" :title="t('groups.newGroup')" @click="handleCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          </Button>
        </div>

        <div v-if="store.groups.length === 0" class="px-3 py-6 text-center">
          <Layers class="h-5 w-5 mx-auto mb-2 text-muted-foreground" :stroke-width="1.5" />
          <p class="text-xs font-medium">{{ t('groups.noGroups') }}</p>
          <p class="text-[10px] text-muted-foreground mt-1">{{ t('groups.noGroupsHint') }}</p>
        </div>

        <div v-else class="flex-1 overflow-y-auto overflow-x-hidden py-1 min-h-0">
          <button
            v-for="group in store.groups"
            :key="group.id"
            type="button"
            class="w-full text-left px-3 py-2 flex items-center gap-2 transition-colors cursor-pointer"
            :class="group.id === selectedId ? 'bg-accent' : 'hover:bg-accent/50'"
            @click="selectGroup(group)"
          >
            <span class="flex-1 min-w-0">
              <span class="block text-xs font-medium truncate">{{ group.name }}</span>
              <span class="block text-[10px] text-muted-foreground">{{ t('groups.memberCount', { count: group.member_count }) }}</span>
            </span>
            <Badge :tone="group.color ?? 'neutral'" size="xs" class="shrink-0">{{ group.member_count }}</Badge>
          </button>
        </div>
      </div>

      <!-- Group editor -->
      <div v-if="selectedGroup" class="flex flex-col min-h-0 min-w-0">
        <div class="p-4 space-y-3 border-b border-border/60 shrink-0">
          <div class="flex items-start gap-3">
            <div class="flex-1 min-w-0">
              <label class="block text-xs text-muted-foreground mb-1.5">{{ t('groups.name') }}</label>
              <Input v-model="draftName" :placeholder="t('groups.groupNamePlaceholder')" />
            </div>
            <div>
              <label class="block text-xs text-muted-foreground mb-1.5">{{ t('groups.color') }}</label>
              <div class="flex items-center gap-1.5 h-8">
                <button
                  v-for="color in GROUP_COLORS"
                  :key="color"
                  type="button"
                  class="h-5 w-5 rounded-full border-2 transition-transform cursor-pointer"
                  :class="[
                    draftColor === color ? 'border-foreground scale-110' : 'border-transparent',
                    {
                      primary: 'bg-indigo-600',
                      info: 'bg-violet-600',
                      success: 'bg-emerald-600',
                      warning: 'bg-amber-600',
                      neutral: 'bg-slate-600',
                    }[color],
                  ]"
                  :title="color"
                  @click="draftColor = draftColor === color ? null : color"
                />
              </div>
            </div>
          </div>

          <div>
            <label class="block text-xs text-muted-foreground mb-1.5">{{ t('groups.description') }}</label>
            <Textarea v-model="draftDescription" :placeholder="t('groups.descriptionPlaceholder')" rows="2" />
          </div>
        </div>

        <!-- Members -->
        <div class="flex items-center gap-2 px-4 py-2 border-b border-border/60 shrink-0">
          <span class="text-xs font-semibold">{{ membersLabel }}</span>
          <span class="text-[10px] text-muted-foreground">{{ t('groups.selectedCount', { count: memberIds.length }) }}</span>
          <div class="ml-auto flex items-center gap-2">
            <button
              class="text-[10px] text-primary hover:underline cursor-pointer"
              @click="memberIds = items.map(i => i.id)"
            >{{ t('groups.selectAll') }}</button>
            <button
              class="text-[10px] text-muted-foreground hover:underline cursor-pointer"
              @click="memberIds = []"
            >{{ t('groups.clearAll') }}</button>
          </div>
        </div>

        <div v-if="items.length === 0" class="px-4 py-6 text-center text-xs text-muted-foreground">
          {{ emptyItemsLabel }}
        </div>
        <div v-else class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden divide-y divide-border/50">
          <label
            v-for="item in items"
            :key="item.id"
            class="flex items-center gap-2.5 px-4 py-2 cursor-pointer hover:bg-accent/40 transition-colors"
          >
            <span
              class="flex h-4 w-4 shrink-0 items-center justify-center rounded border transition-colors"
              :class="memberIds.includes(item.id) ? 'bg-primary border-primary text-white' : 'border-border'"
            >
              <Check v-if="memberIds.includes(item.id)" class="h-3 w-3" :stroke-width="3" />
            </span>
            <input type="checkbox" class="sr-only" :checked="memberIds.includes(item.id)" @change="toggleMember(item.id)" />
            <span class="min-w-0 flex-1">
              <span class="block text-xs font-medium font-mono truncate">{{ item.name }}</span>
              <span v-if="item.description" class="block text-[10px] text-muted-foreground truncate">{{ item.description }}</span>
            </span>
          </label>
        </div>
      </div>

      <div v-else class="flex items-center justify-center text-xs text-muted-foreground">
        {{ t('groups.selectGroupHint') }}
      </div>
    </div>

    <template #footer>
      <div v-if="selectedGroup" class="mr-auto flex items-center gap-2">
        <Button variant="destructive-ghost" @click="handleDelete">
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('groups.deleteGroup') }}
        </Button>
        <Button variant="outline" :disabled="applyingAll" @click="handleApplyToAll">
          <FolderCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('groups.applyToAll') }}
        </Button>
      </div>
      <Button :disabled="saving || !selectedGroup || !draftName.trim()" @click="handleSave">
        <Loader2 v-if="saving" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
        {{ saving ? t('groups.saving') : t('groups.save') }}
      </Button>
    </template>
  </Modal>
</template>
