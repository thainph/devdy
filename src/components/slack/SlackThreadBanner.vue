<script setup lang="ts">
// Status banners above the messages, driven by the thread row and the
// `slack-threads://progress` events:
//   • a job is queued / running → progress bar + phase ("Downloading files 3/8");
//   • error → red banner with the i18n text of the code, the English detail in
//     small print, and Retry (not for dm_unsupported / legacy_no_url);
//   • warnings of the last job → amber banner, one line per code;
//   • legacy snapshot not re-fetched yet → neutral banner.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, AlertTriangle, Archive, Loader2, RotateCw, Server } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { isBusy, type SlackThreadProgress, type SlackThreadSummary } from '@/stores/slackThreads'
import { useSlackText } from './useSlackText'

const props = defineProps<{
  thread: SlackThreadSummary
  progress: SlackThreadProgress | null
}>()

const emit = defineEmits<{ retry: []; openMcp: [] }>()

const { t } = useI18n()
const { errorText, warningText, progressLabel } = useSlackText()

const NO_RETRY = new Set(['dm_unsupported', 'legacy_no_url'])
const TOKEN_CODES = new Set(['no_token', 'no_token_for_workspace', 'invalid_auth'])

const busy = computed(() => isBusy(props.thread.fetchStatus))

/** 0–100 for a determinate bar, null for an indeterminate one. */
const percent = computed(() => {
  const p = props.progress
  if (!p || p.status !== props.thread.fetchStatus || !p.total) return null
  return Math.min(100, Math.round((p.done / p.total) * 100))
})

const errorCodeShown = computed(() => props.thread.fetchErrorCode)
const errorMessage = computed(() => errorText(props.thread.fetchErrorCode, props.thread.fetchError))
/** The English detail, unless the i18n text already embeds it. */
const errorDetail = computed(() => {
  const d = props.thread.fetchError
  return d && !errorMessage.value.includes(d) ? d : null
})
</script>

<template>
  <div class="shrink-0 space-y-px">
    <div v-if="busy" class="border-b border-border/60 bg-primary/5 px-5 py-2">
      <div class="flex items-center gap-2 text-xs text-foreground">
        <Loader2 class="h-3.5 w-3.5 shrink-0 animate-spin text-primary" :stroke-width="2" />
        <span class="truncate">{{ progressLabel(thread, progress) }}</span>
      </div>
      <div class="mt-1.5 h-1 overflow-hidden rounded-full bg-muted">
        <div
          v-if="percent !== null"
          class="h-full rounded-full bg-primary transition-[width] duration-300"
          :style="{ width: `${percent}%` }"
        />
        <div v-else class="h-full w-1/3 animate-pulse rounded-full bg-primary/60" />
      </div>
    </div>

    <div
      v-else-if="thread.fetchStatus === 'error'"
      class="flex items-start gap-2 border-b border-destructive/30 bg-destructive/10 px-5 py-2 text-xs text-foreground"
    >
      <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="2" />
      <div class="min-w-0 flex-1">
        <p>{{ errorMessage }}</p>
        <p v-if="errorDetail" class="mt-0.5 break-words font-mono text-[10px] text-muted-foreground">
          {{ errorDetail }}
        </p>
      </div>
      <Button
        v-if="errorCodeShown && TOKEN_CODES.has(errorCodeShown)"
        variant="outline"
        size="xs"
        class="shrink-0"
        @click="emit('openMcp')"
      >
        <Server class="h-3 w-3" :stroke-width="1.75" />
        {{ t('slackThread.banner.openMcp') }}
      </Button>
      <Button
        v-if="!errorCodeShown || !NO_RETRY.has(errorCodeShown)"
        variant="outline"
        size="xs"
        class="shrink-0"
        @click="emit('retry')"
      >
        <RotateCw class="h-3 w-3" :stroke-width="1.75" />
        {{ t('slackThread.banner.retry') }}
      </Button>
    </div>

    <div
      v-if="!busy && thread.fetchStatus === 'ready' && thread.warnings.length"
      class="flex items-start gap-2 border-b border-amber-500/30 bg-amber-500/10 px-5 py-2 text-xs text-foreground"
    >
      <AlertTriangle class="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-500" :stroke-width="2" />
      <ul class="min-w-0 flex-1 space-y-0.5">
        <li v-for="w in thread.warnings" :key="w">{{ warningText(w) }}</li>
      </ul>
    </div>

    <div
      v-if="thread.legacy"
      class="flex items-center gap-2 border-b border-border/60 bg-muted/40 px-5 py-2 text-xs text-muted-foreground"
    >
      <Archive class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      <span>{{ t('slackThread.legacy.banner') }}</span>
    </div>
  </div>
</template>
