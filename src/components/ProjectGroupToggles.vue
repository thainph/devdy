<script setup lang="ts">
// Group rows inside a project's Skills / Rules tab: one switch per group, with a mixed state for
// bundles that are only partly applied. Used by both tabs, so it takes the states as a prop.
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'
import { Badge, Card, ToggleSwitch } from '@/components/ui'
import { Layers } from 'lucide-vue-next'
import type { GroupKind, ProjectGroupState } from '@/stores/groups'

defineProps<{
  kind: GroupKind
  groups: ProjectGroupState[]
  /** Group id currently being applied/removed, or null. */
  toggling: string | null
}>()

const emit = defineEmits<{
  toggle: [group: ProjectGroupState]
  reapply: [group: ProjectGroupState]
}>()

const { t } = useI18n()
</script>

<template>
  <Card>
    <template #header>
      <Layers class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('groups.projectGroupsTitle') }}</span>
      <RouterLink
        :to="kind === 'skill' ? '/skills' : '/rules'"
        class="ml-auto text-[11px] text-primary hover:underline"
      >{{ t('projectDetail.manage') }}</RouterLink>
    </template>

    <div v-if="groups.length === 0" class="px-4 py-5 text-center text-xs text-muted-foreground">
      {{ t('groups.projectGroupsEmpty') }}
      <RouterLink
        :to="kind === 'skill' ? '/skills' : '/rules'"
        class="text-primary hover:underline"
      >{{ t('groups.projectGroupsEmptyLink') }}</RouterLink>
    </div>

    <div v-else class="divide-y divide-border/50">
      <div v-for="group in groups" :key="group.group_id" class="flex items-center gap-3 px-4 py-3">
        <div class="flex-1 min-w-0">
          <div class="flex items-center gap-1.5 flex-wrap">
            <p class="text-xs font-medium truncate">{{ group.name }}</p>
            <Badge :tone="group.color ?? 'neutral'" size="xs" class="shrink-0">
              {{ group.applied_count }}/{{ group.member_count }}
            </Badge>
            <button
              v-if="group.state === 'mixed' && group.member_count > 0"
              class="text-[10px] text-primary hover:underline cursor-pointer"
              :disabled="toggling === group.group_id"
              @click="emit('reapply', group)"
            >{{ t('groups.reapply') }}</button>
          </div>
          <p v-if="group.member_count === 0" class="text-[10px] text-muted-foreground mt-0.5">
            {{ t('groups.groupEmptyHint') }}
          </p>
          <p v-else-if="group.description" class="text-[10px] text-muted-foreground truncate mt-0.5">
            {{ group.description }}
          </p>
        </div>

        <ToggleSwitch
          :checked="group.state === 'mixed' ? 'mixed' : group.state === 'on'"
          :disabled="toggling === group.group_id || group.member_count === 0"
          :loading="toggling === group.group_id"
          :label="group.state === 'off' ? t('groups.enableGroup') : t('groups.disableGroup')"
          @toggle="emit('toggle', group)"
        />
      </div>
    </div>
  </Card>
</template>
