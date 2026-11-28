<script setup lang="ts">
// The one settings button of GitHub Mentions (icon → modal, InboxOptionsButton):
// the filters first, then the display settings; the footer clears the filters.
import InboxOptionsButton from '@/components/inbox/InboxOptionsButton.vue'
import MentionFiltersPanel from '@/components/githubMentions/MentionFiltersPanel.vue'
import MentionDisplayPanel from '@/components/githubMentions/MentionDisplayPanel.vue'
import type { MentionFilterDimension, MentionFilterOption, MentionFilters } from '@/lib/githubMentions'

defineProps<{
  options: Record<MentionFilterDimension, MentionFilterOption[]>
  modelValue: MentionFilters
  activeCount: number
  optionLabel: (dimension: MentionFilterDimension, option: MentionFilterOption) => string
}>()

const emit = defineEmits<{
  update: [dimension: MentionFilterDimension, values: string[]]
  clear: []
  'edit-prompt': []
}>()

function editPrompt(close: () => void) {
  close()
  emit('edit-prompt')
}
</script>

<template>
  <InboxOptionsButton v-slot="{ close }" :active-count="activeCount" @clear="emit('clear')">
    <MentionFiltersPanel
      :options="options"
      :model-value="modelValue"
      :option-label="optionLabel"
      @update="(d, values) => emit('update', d, values)"
    />
    <MentionDisplayPanel @edit-prompt="editPrompt(close)" />
  </InboxOptionsButton>
</template>
