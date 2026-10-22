<script setup lang="ts">
// "+ Watch" box of the Pulse header (§6.1): paste a Slack thread link, a
// GitHub issue link or owner/repo#n, pick the project, submit. Errors (§7)
// show inline under the field; the popover closes on success.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { onClickOutside } from '@vueuse/core'
import { Plus } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { usePulseWatch } from '@/composables/usePulseWatch'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { usePopoverFocus } from '@/composables/usePopoverFocus'

const { t } = useI18n()
const { store, errorMessage } = usePulseWatch()

const open = ref(false)
const url = ref('')
const projectId = ref<string | null>(null)
const error = ref<string | null>(null)
const saving = ref(false)
const wrap = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)

// The project dropdown is teleported to <body>: picking in it is not "outside".
onClickOutside(wrap, () => { open.value = false }, { ignore: ['[data-app-select-dropdown]'] })
useEscapeClose(open)
usePopoverFocus(open, wrap, panel)

function toggle() {
  open.value = !open.value
  error.value = null
}

async function submit() {
  const link = url.value.trim()
  if (!link) return
  if (!projectId.value) {
    error.value = t('pulse.errors.project_required')
    return
  }
  saving.value = true
  error.value = null
  try {
    await store.watch(link, projectId.value, 'paste')
    url.value = ''
    open.value = false
  } catch (e) {
    error.value = errorMessage(e)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div ref="wrap" class="relative">
    <Button variant="outline" size="sm" aria-haspopup="dialog" :aria-expanded="open" @click="toggle">
      <Plus class="h-3.5 w-3.5" :stroke-width="2" />
      {{ t('pulse.add.button') }}
    </Button>
    <form
      v-if="open"
      ref="panel"
      class="absolute right-0 top-full z-50 mt-2 w-96 space-y-2.5 rounded-lg border border-border/60 bg-popover p-3 shadow-xl shadow-black/30"
      @submit.prevent="submit"
    >
      <Input v-model="url" :placeholder="t('pulse.add.placeholder')" />
      <div class="flex items-center gap-2">
        <div class="min-w-0 flex-1">
          <ProjectSelect v-model="projectId" variant="default" size="sm" />
        </div>
        <Button type="submit" size="sm" :disabled="saving || !url.trim()">{{ t('pulse.add.submit') }}</Button>
      </div>
      <p v-if="error" class="text-xs text-destructive">{{ error }}</p>
    </form>
  </div>
</template>
