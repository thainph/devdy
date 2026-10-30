<script setup lang="ts">
// Dialogs of the board's AI actions (useFocusAiActions), mounted once by
// FocusView: pick a project before "Start with AI" on a todo that has none.
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Modal } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { useFocusAiActions } from './useFocusAiActions'

const { t } = useI18n()
const { pickProjectFor, startInProject } = useFocusAiActions()

const picked = ref<string | null>(null)
watch(pickProjectFor, () => (picked.value = null))

function close() {
  pickProjectFor.value = null
}
function confirm() {
  if (pickProjectFor.value && picked.value) void startInProject(pickProjectFor.value, picked.value)
}
</script>

<template>
  <Modal :open="!!pickProjectFor" :title="t('focus.ai.pickProjectTitle')" size="sm" @close="close">
    <div class="space-y-3 p-4">
      <p class="text-xs text-muted-foreground">{{ t('focus.ai.pickProjectHint') }}</p>
      <ProjectSelect v-model="picked" variant="default" size="sm" />
    </div>
    <template #footer>
      <Button variant="ghost" @click="close">{{ t('common.cancel') }}</Button>
      <Button :disabled="!picked" @click="confirm">{{ t('focus.ai.start') }}</Button>
    </template>
  </Modal>
</template>
