<script setup lang="ts">
// The project picker of the detail windows: "No project" + every project, with
// a folder icon. `outline` (default) is the footer control of the read view;
// the note / todo edit form uses the plain field look.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { FolderOpen } from 'lucide-vue-next'
import { AppSelect } from '@/components/ui'
import { useProjectsStore } from '@/stores/projects'

const props = withDefaults(
  defineProps<{
    /** Project id, or null for none. */
    modelValue: string | null
    variant?: 'default' | 'outline'
  }>(),
  { variant: 'outline' },
)

const emit = defineEmits<{ 'update:modelValue': [value: string | null] }>()

const { t } = useI18n()
const projects = useProjectsStore()

const NO_PROJECT = ''

const options = computed(() => [
  { value: NO_PROJECT, label: t('item.noProject') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })),
])
</script>

<template>
  <AppSelect
    :model-value="props.modelValue ?? NO_PROJECT"
    :variant="variant"
    :options="options"
    :placeholder="t('item.noProject')"
    @update:model-value="emit('update:modelValue', $event || null)"
  >
    <template #leading>
      <FolderOpen class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
    </template>
  </AppSelect>
</template>
