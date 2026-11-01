<script setup lang="ts">
// Liveness notices for a running session (backend `run:attention`): the turn has
// been quiet for long, a tool has been running for long, the session bridge
// stopped responding / never started, or a prompt has been waiting for long.
// The backend never stops a session or answers a prompt on these — only the user
// can tell a long job from a stuck one — so this offers the stop action and
// otherwise stays out of the way. Notices clear themselves when things move.
import { useI18n } from 'vue-i18n'
import { Square } from 'lucide-vue-next'
import { Banner } from '@/components/ui'
import type { RunAttention } from '@/stores/liveRuns'

defineProps<{ notices: RunAttention[] }>()
const emit = defineEmits<{ stop: [] }>()
const { t } = useI18n()

function text(n: RunAttention): string {
  const mins = n.params.mins ?? 0
  if (n.kind === 'quiet') return t('run.attentionQuiet', { mins })
  if (n.kind === 'long_tool') return t('run.attentionLongTool', { tool: n.params.tool ?? '', mins })
  if (n.kind === 'unresponsive') return t('run.attentionUnresponsive', { mins })
  if (n.kind === 'unmonitored') return t('run.attentionUnmonitored', { mins })
  if (n.kind === 'input_stalled') return t('run.attentionInputStalled')
  if (n.kind === 'not_started') return t('run.attentionNotStarted', { secs: n.params.secs ?? 0 })
  if (n.kind === 'permission_waiting') {
    return t('run.attentionPermissionWaiting', { tool: n.params.tool ?? '', mins })
  }
  return n.message
}
</script>

<template>
  <Banner v-if="notices.length" tone="warning" class="mb-2">
    <div class="space-y-0.5">
      <p v-for="n in notices" :key="n.key">{{ text(n) }}</p>
    </div>
    <template #actions>
      <button
        type="button"
        class="inline-flex shrink-0 items-center gap-1 rounded px-2 py-0.5 font-medium text-warning hover:bg-warning/20 cursor-pointer"
        :title="t('run.cancelRunningTurn')"
        @click="emit('stop')"
      >
        <Square class="h-3 w-3" :stroke-width="2" fill="currentColor" />
        {{ t('run.attentionStop') }}
      </button>
    </template>
  </Banner>
</template>
