<script setup lang="ts">
// "Already exists" prompt of the File Transfer window: replace (a folder is
// merged), keep both (numbered name) or skip, optionally for every remaining
// clash of the same copy. Closing it cancels the copy.
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Modal } from '@/components/ui'
import type { ConflictAnswer, ConflictChoice } from '@/lib/fileTransferCore'

const props = defineProps<{ name: string; isDir: boolean; open: boolean }>()
const emit = defineEmits<{ answer: [answer: ConflictAnswer | null] }>()

const { t } = useI18n()
const applyToAll = ref(false)

watch(
  () => props.open,
  (open) => {
    if (open) applyToAll.value = false
  },
)

function pick(choice: ConflictChoice) {
  emit('answer', { choice, applyToAll: applyToAll.value })
}
</script>

<template>
  <Modal :open="open" :title="t('fileTransfer.conflict.title')" size="sm" :dismiss-on-overlay="false" @close="emit('answer', null)">
    <div class="space-y-3 px-4 py-4 text-sm">
      <p class="break-all">
        {{ isDir ? t('fileTransfer.conflict.folderExists', { name }) : t('fileTransfer.conflict.fileExists', { name }) }}
      </p>
      <p class="text-xs text-muted-foreground">
        {{ isDir ? t('fileTransfer.conflict.folderHint') : t('fileTransfer.conflict.fileHint') }}
      </p>
      <label class="flex items-center gap-2 text-xs">
        <input v-model="applyToAll" type="checkbox" class="accent-primary">
        {{ t('fileTransfer.conflict.applyToAll') }}
      </label>
    </div>
    <template #footer>
      <Button variant="ghost" @click="emit('answer', null)">{{ t('common.cancel') }}</Button>
      <span class="flex-1" />
      <Button variant="outline" @click="pick('skip')">{{ t('fileTransfer.conflict.skip') }}</Button>
      <Button variant="outline" @click="pick('rename')">{{ t('fileTransfer.conflict.keepBoth') }}</Button>
      <Button @click="pick('overwrite')">
        {{ isDir ? t('fileTransfer.conflict.merge') : t('fileTransfer.conflict.replace') }}
      </Button>
    </template>
  </Modal>
</template>
