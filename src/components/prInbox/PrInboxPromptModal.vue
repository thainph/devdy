<script setup lang="ts">
// Editor for the two PR review prompts: the one a review run starts with
// (`review`) and the follow-up "Re-review" sends to the same session after its
// task file is re-fetched (`rereview`), stored as pr_inbox.<kind>_prompt(_id).
import { useI18n } from 'vue-i18n'
import { usePrInboxStore, type PrPromptKind } from '@/stores/prInbox'
import PromptConfigModal from '@/components/PromptConfigModal.vue'

const props = defineProps<{ open: boolean; kind: PrPromptKind }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = usePrInboxStore()
</script>

<template>
  <PromptConfigModal
    :open="open"
    :title="t(`prInbox.prompt.${kind}.title`)"
    :hint="t(`prInbox.prompt.${kind}.hint`)"
    :load="() => store.loadPromptConfig(props.kind)"
    :save-text="p => store.savePrompt(props.kind, p)"
    :save-id="id => store.savePromptId(props.kind, id)"
    @close="emit('close')"
  />
</template>
