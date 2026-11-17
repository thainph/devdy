<script setup lang="ts">
// Editor for the two PR review prompts: the one a review run starts with
// (`review`) and the follow-up "Re-review" sends to the same session after its
// task file is re-fetched (`rereview`). Either a Settings → Prompt Templates
// entry (pr_inbox.<kind>_prompt_id, managed with the rest of the library) or
// custom text (pr_inbox.<kind>_prompt); both are backend settings so every
// window shares them. A blank custom prompt falls back to the default.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePrInboxStore, type PrPromptKind } from '@/stores/prInbox'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useToast } from '@/composables/useToast'
import { parseSavedPrompts, promptLabel } from '@/lib/savedPrompts'
import { Modal, Button, Textarea } from '@/components/ui'
import AppSelect from '@/components/AppSelect.vue'

const props = defineProps<{ open: boolean; kind: PrPromptKind }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = usePrInboxStore()
const appSettings = useAppSettingsStore()
const { toast } = useToast()
const draft = ref('')
// '' = custom text; otherwise a saved prompt id.
const source = ref('')
const saving = ref(false)

const savedPrompts = computed(() => parseSavedPrompts(appSettings.settings?.saved_prompts))
const sourceOptions = computed(() => [
  { value: '', label: t('prInbox.prompt.custom') },
  ...savedPrompts.value.map(p => ({ value: p.id, label: promptLabel(p) })),
])
const selectedSaved = computed(() => savedPrompts.value.find(p => p.id === source.value) ?? null)

// Re-read on open: the other inbox window or Settings may have changed them.
watch(() => [props.open, props.kind] as const, async ([o]) => {
  if (!o) return
  draft.value = ''
  source.value = ''
  try {
    const [config] = await Promise.all([store.loadPromptConfig(props.kind), appSettings.refresh()])
    draft.value = config.prompt
    // A link to a prompt deleted from the library shows as custom text.
    if (config.saved_prompt_id && savedPrompts.value.some(p => p.id === config.saved_prompt_id)) {
      source.value = config.saved_prompt_id
    }
  } catch (e) {
    toast.error(String(e))
  }
}, { immediate: true })

async function save(reset = false) {
  saving.value = true
  try {
    if (reset) {
      await store.savePromptId(props.kind, null)
      await store.savePrompt(props.kind, '')
    } else if (selectedSaved.value) {
      await store.savePromptId(props.kind, selectedSaved.value.id)
    } else {
      await store.savePromptId(props.kind, null)
      await store.savePrompt(props.kind, draft.value)
    }
    emit('close')
  } catch (e) {
    toast.error(t('prInbox.prompt.saveFailed', { error: String(e) }))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Modal :open="open" :title="t(`prInbox.prompt.${kind}.title`)" size="md" @close="emit('close')">
    <div class="flex flex-col gap-3 px-4 py-4">
      <p class="text-xs text-muted-foreground">{{ t(`prInbox.prompt.${kind}.hint`) }}</p>
      <div class="flex flex-col gap-1">
        <span class="text-xs font-medium text-foreground">{{ t('prInbox.prompt.source') }}</span>
        <AppSelect v-model="source" :options="sourceOptions" :disabled="saving" class="w-full" />
        <p v-if="!savedPrompts.length" class="text-[11px] text-muted-foreground">
          {{ t('prInbox.prompt.noSaved') }}
        </p>
      </div>
      <Textarea
        v-if="selectedSaved"
        :model-value="selectedSaved.body"
        rows="10"
        class="font-mono text-xs"
        readonly
        :title="t('prInbox.prompt.savedReadonly')"
      />
      <Textarea v-else v-model="draft" rows="10" class="font-mono text-xs" :disabled="saving" />
      <p v-if="selectedSaved" class="text-[11px] text-muted-foreground">{{ t('prInbox.prompt.savedReadonly') }}</p>
    </div>
    <template #footer>
      <Button variant="ghost" :disabled="saving" @click="save(true)">{{ t('prInbox.prompt.reset') }}</Button>
      <Button variant="outline" :disabled="saving" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button :disabled="saving || (!selectedSaved && !draft.trim())" @click="save()">{{ t('common.save') }}</Button>
    </template>
  </Modal>
</template>
