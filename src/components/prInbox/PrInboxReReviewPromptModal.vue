<script setup lang="ts">
// The follow-up prompt "Re-review" sends to a PR's existing review session
// (after its task file is re-fetched). Stored as a backend setting
// (pr_inbox.rereview_prompt) so both inbox windows share it; a blank prompt
// falls back to the built-in default.
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePrInboxStore } from '@/stores/prInbox'
import { useToast } from '@/composables/useToast'
import { Modal, Button, Textarea } from '@/components/ui'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = usePrInboxStore()
const { toast } = useToast()
const draft = ref('')
const saving = ref(false)

// Re-read on open: the other inbox window may have changed it.
watch(() => props.open, async (o) => {
  if (!o) return
  draft.value = ''
  try {
    draft.value = await store.loadReReviewPrompt()
  } catch (e) {
    toast.error(String(e))
  }
}, { immediate: true })

async function save(prompt: string) {
  saving.value = true
  try {
    await store.saveReReviewPrompt(prompt)
    emit('close')
  } catch (e) {
    toast.error(t('prInbox.reReviewPrompt.saveFailed', { error: String(e) }))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Modal :open="open" :title="t('prInbox.reReviewPrompt.title')" size="md" @close="emit('close')">
    <div class="flex flex-col gap-3 px-4 py-4">
      <p class="text-xs text-muted-foreground">{{ t('prInbox.reReviewPrompt.hint') }}</p>
      <Textarea v-model="draft" rows="10" class="font-mono text-xs" :disabled="saving" />
    </div>
    <template #footer>
      <Button variant="ghost" :disabled="saving" @click="save('')">{{ t('prInbox.reReviewPrompt.reset') }}</Button>
      <Button variant="outline" :disabled="saving" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button :disabled="saving || !draft.trim()" @click="save(draft)">{{ t('common.save') }}</Button>
    </template>
  </Modal>
</template>
