<script setup lang="ts">
// The small status mark of a Slack thread row (sidebar + composer picker):
// spinner while a job is queued / running (tooltip: phase + done/total), red
// alert on error (tooltip: the error), amber dot when the last job left warnings.
// Nothing for a clean `ready` thread.
import { computed } from 'vue'
import { AlertCircle, Loader2 } from 'lucide-vue-next'
import { isBusy, type SlackThreadProgress, type SlackThreadSummary } from '@/stores/slackThreads'
import { useSlackText } from './useSlackText'

const props = defineProps<{
  thread: Pick<SlackThreadSummary, 'fetchStatus' | 'fetchErrorCode' | 'fetchError' | 'warnings'>
  progress?: SlackThreadProgress | null
}>()

const { errorText, warningText, progressLabel } = useSlackText()

const busy = computed(() => isBusy(props.thread.fetchStatus))
const isError = computed(() => props.thread.fetchStatus === 'error')
const hasWarning = computed(
  () => props.thread.fetchStatus === 'ready' && props.thread.warnings.length > 0,
)
</script>

<template>
  <span v-if="busy" class="flex shrink-0" :title="progressLabel(thread, progress)">
    <Loader2 class="h-3.5 w-3.5 animate-spin text-primary" :stroke-width="2" />
  </span>
  <span
    v-else-if="isError"
    class="flex shrink-0"
    :title="errorText(thread.fetchErrorCode, thread.fetchError)"
  >
    <AlertCircle class="h-3.5 w-3.5 text-destructive" :stroke-width="2" />
  </span>
  <span
    v-else-if="hasWarning"
    class="h-2 w-2 shrink-0 rounded-full bg-amber-500"
    :title="thread.warnings.map(warningText).join('\n')"
  />
</template>
