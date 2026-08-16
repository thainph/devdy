<script setup lang="ts">
// Global text-input dialog host. Mount once (in App.vue); triggered from anywhere
// via usePrompt().prompt(). Matches the app's design system, like ConfirmModal.
import { nextTick, ref, watch } from 'vue'
import { Modal, Button, Input } from '@/components/ui'
import { usePrompt } from '@/composables/usePrompt'

const { state, respond } = usePrompt()
const inputEl = ref<InstanceType<typeof Input> | null>(null)

// Focus (and optionally select) the field when the dialog opens.
watch(
  () => state.open,
  async (open) => {
    if (!open) return
    await nextTick()
    const el = (inputEl.value?.$el ?? null) as HTMLInputElement | null
    if (!el) return
    el.focus()
    if (state.selectAll) el.select()
  },
)

const disabled = () => state.value.trim().length === 0

function submit() {
  if (disabled()) return
  respond(state.value.trim())
}
</script>

<template>
  <Modal :open="state.open" :title="state.title" size="sm" @close="respond(null)">
    <div class="px-4 py-4 space-y-1.5">
      <label v-if="state.label" class="block text-xs font-medium text-muted-foreground">{{ state.label }}</label>
      <Input
        ref="inputEl"
        v-model="state.value"
        :placeholder="state.placeholder"
        @keydown.enter.prevent="submit"
        @keydown.esc.prevent="respond(null)"
      />
    </div>
    <template #footer>
      <Button variant="outline" @click="respond(null)">{{ state.cancelLabel }}</Button>
      <Button variant="primary" :disabled="disabled()" @click="submit">{{ state.confirmLabel }}</Button>
    </template>
  </Modal>
</template>
