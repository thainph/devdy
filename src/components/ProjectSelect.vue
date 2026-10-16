<script setup lang="ts">
// The project picker: "No project" + every project, with a folder icon.
// `outline` at `xs` is the header control of the Slack / Notes / Todos windows'
// detail pane; `default` is the plain field look of forms (Add thread dialog).
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
    size?: 'xs' | 'sm'
  }>(),
  { variant: 'outline', size: 'sm' },
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
    :size="size"
    :options="options"
    :placeholder="t('item.noProject')"
    @update:model-value="emit('update:modelValue', $event || null)"
  >
    <template #leading>
      <FolderOpen
        class="shrink-0 text-muted-foreground"
        :class="size === 'xs' ? 'h-3 w-3' : 'h-3.5 w-3.5'"
        :stroke-width="1.75"
      />
    </template>
  </AppSelect>
</template>
