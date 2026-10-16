<script setup lang="ts">
// "+ Add thread": paste a Slack thread link (+ optional project). The link is
// checked client-side first (spec §4) for an early hint; the backend's error
// code is the authority and shows inline. Without any Slack token the dialog
// warns (and links to the MCP settings) but still lets the thread be added.
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, Server } from 'lucide-vue-next'
import { Button, Input, Modal } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { checkThreadLink } from '@/lib/slackThreadUtils'
import {
  errorCode,
  useSlackThreadsStore,
  type AddSlackThreadResult,
} from '@/stores/slackThreads'
import { useSlackText } from './useSlackText'

const props = defineProps<{
  open: boolean
  /** Project preselected when the dialog opens. */
  defaultProjectId: string | null
}>()

const emit = defineEmits<{
  close: []
  added: [result: AddSlackThreadResult]
  openMcp: []
}>()

const { t } = useI18n()
const store = useSlackThreadsStore()
const { errorText } = useSlackText()

const url = ref('')
const projectId = ref<string | null>(null)
const submitting = ref(false)
const serverError = ref<string | null>(null)
const noToken = ref(false)
const formEl = ref<HTMLElement | null>(null)

watch(
  () => props.open,
  async (open) => {
    if (!open) return
    url.value = ''
    projectId.value = props.defaultProjectId
    serverError.value = null
    submitting.value = false
    noToken.value = false
    await nextTick()
    formEl.value?.querySelector('input')?.focus()
    try {
      noToken.value = (await store.tokenStatus()).state === 'none'
    } catch {
      /* command unavailable: no warning */
    }
  },
  { immediate: true },
)

watch(url, () => {
  serverError.value = null
})

/** Early, client-side hint; empty input shows nothing. */
const linkCheck = computed(() => (url.value.trim() ? checkThreadLink(url.value) : null))
const clientError = computed(() =>
  linkCheck.value && linkCheck.value !== 'ok' ? errorText(linkCheck.value) : null,
)
const canSubmit = computed(() => linkCheck.value === 'ok' && !submitting.value)

async function submit() {
  if (!canSubmit.value) return
  submitting.value = true
  serverError.value = null
  try {
    const res = await store.add(url.value.trim(), projectId.value)
    emit('added', res)
  } catch (e) {
    const { code, detail } = errorCode(e)
    serverError.value = errorText(code, detail)
  } finally {
    submitting.value = false
  }
}

// Enter in the link field submits (the project picker keeps Enter for itself).
function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter' && (e.target as HTMLElement | null)?.tagName === 'INPUT') {
    e.preventDefault()
    void submit()
  }
}
</script>

<template>
  <Modal :open="open" :title="t('slackThread.add.title')" size="md" @close="emit('close')">
    <div ref="formEl" class="space-y-3 px-4 py-4" @keydown="onKeydown">
      <div class="space-y-1">
        <label class="text-xs font-medium text-foreground">{{ t('slackThread.add.urlLabel') }}</label>
        <Input v-model="url" :placeholder="t('slackThread.add.urlPlaceholder')" spellcheck="false" />
        <p v-if="clientError || serverError" class="text-[11px] text-destructive">
          {{ serverError ?? clientError }}
        </p>
        <p v-else class="text-[11px] text-muted-foreground">{{ t('slackThread.add.urlHint') }}</p>
      </div>

      <div class="space-y-1">
        <label class="text-xs font-medium text-foreground">{{ t('slackThread.add.project') }}</label>
        <ProjectSelect v-model="projectId" variant="default" />
      </div>

      <div
        v-if="noToken"
        class="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-foreground"
      >
        <AlertTriangle class="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-500" :stroke-width="2" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <p>{{ t('slackThread.add.noToken') }}</p>
          <Button variant="outline" size="xs" @click="emit('openMcp')">
            <Server class="h-3 w-3" :stroke-width="1.75" />
            {{ t('slackThread.banner.openMcp') }}
          </Button>
        </div>
      </div>
    </div>

    <template #footer>
      <Button variant="ghost" size="sm" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button size="sm" :disabled="!canSubmit" @click="submit">{{ t('slackThread.add.submit') }}</Button>
    </template>
  </Modal>
</template>
