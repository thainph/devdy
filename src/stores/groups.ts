import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'
import type { ApplyAllOutcome } from '@/stores/rules'

export type GroupKind = 'skill' | 'rule'

/** Badge tones a group can be tinted with — a fixed set, so group chips stay on the design system. */
export const GROUP_COLORS = ['primary', 'info', 'success', 'warning', 'neutral'] as const
export type GroupColor = (typeof GROUP_COLORS)[number]

export interface Group {
  id: string
  name: string
  description: string
  color: GroupColor | null
  position: number
  member_count: number
  updated_at: string
}

/** A group as seen from one project: how much of it is applied, and whether the project runs it. */
export interface ProjectGroupState {
  group_id: string
  name: string
  description: string
  color: GroupColor | null
  member_count: number
  applied_count: number
  linked: boolean
  state: 'on' | 'mixed' | 'off'
}

export interface GroupItemFailure {
  item_id: string
  item_name: string
  error: string
}

export interface GroupApplyOutcome {
  applied: number
  failures: GroupItemFailure[]
}

export interface GroupDisableOutcome {
  removed: number
  kept_manual: number
  kept_other_group: number
  failures: GroupItemFailure[]
}

/**
 * Skill groups and rule groups are the same store shape over a different `kind`, so build both
 * from one factory instead of copying the file (see `commands/groups.rs` for the mirror image).
 */
function createGroupsStore(kind: GroupKind) {
  return defineStore(`${kind}Groups`, () => {
    const groups = ref<Group[]>([])
    /** item id -> group ids it belongs to, for badges in the list views. */
    const itemGroups = ref<Record<string, string[]>>({})
    const loading = ref(false)
    const error = ref<string | null>(null)

    async function fetchGroups() {
      loading.value = true
      error.value = null
      try {
        groups.value = await invoke<Group[]>('list_groups', { kind })
        const pairs = await invoke<[string, string][]>('get_item_groups', { kind })
        const map: Record<string, string[]> = {}
        for (const [itemId, groupId] of pairs) (map[itemId] ??= []).push(groupId)
        itemGroups.value = map
      } catch (e) {
        error.value = String(e)
      } finally {
        loading.value = false
      }
    }

    async function createGroup(payload: { name: string; description: string; color: GroupColor | null }): Promise<Group> {
      const group = await invoke<Group>('create_group', { kind, payload })
      await fetchGroups()
      return group
    }

    async function updateGroup(payload: { id: string; name: string; description: string; color: GroupColor | null }): Promise<Group> {
      const group = await invoke<Group>('update_group', { kind, payload })
      await fetchGroups()
      return group
    }

    async function deleteGroup(groupId: string): Promise<void> {
      await invoke('delete_group', { kind, groupId })
      await fetchGroups()
    }

    async function getMembers(groupId: string): Promise<string[]> {
      return invoke<string[]>('get_group_members', { kind, groupId })
    }

    /** Overwrite membership; the backend propagates the diff to every project running the group. */
    async function setMembers(groupId: string, itemIds: string[]): Promise<GroupApplyOutcome> {
      const outcome = await invoke<GroupApplyOutcome>('set_group_members', { kind, groupId, itemIds })
      await fetchGroups()
      return outcome
    }

    async function applyGroupToAllProjects(groupId: string): Promise<ApplyAllOutcome> {
      return invoke<ApplyAllOutcome>('apply_group_to_all_projects', { kind, groupId })
    }

    return {
      groups, itemGroups, loading, error,
      fetchGroups, createGroup, updateGroup, deleteGroup,
      getMembers, setMembers, applyGroupToAllProjects,
    }
  })
}

export const useSkillGroupsStore = createGroupsStore('skill')
export const useRuleGroupsStore = createGroupsStore('rule')

export function useGroupsStore(kind: GroupKind) {
  return kind === 'skill' ? useSkillGroupsStore() : useRuleGroupsStore()
}
