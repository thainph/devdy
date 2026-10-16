<script setup lang="ts">
// Header of the thread pane: the title (click / F2 → inline rename), then
// `#channel · workspace`, and the thread actions — whole-thread "Show original"
// toggle, Refresh (spinner + disabled while a job runs) and Open in Slack.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ExternalLink, Languages, PencilLine, RotateCw } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import InlineRenameInput from '@/components/InlineRenameInput.vue'
import { formatFullDate } from '@/lib/slackThreadUtils'
import { isBusy, type SlackThreadSummary } from '@/stores/slackThreads'

const props = defineProps<{
  thread: SlackThreadSummary
  renaming: boolean
  showOriginal: boolean
  refreshing: boolean
}>()

const emit = defineEmits<{
  startRename: []
  commitRename: [title: string]
  cancelRename: []
  toggleOriginal: []
  refresh: []
  openInSlack: []
}>()

const { t } = useI18n()

const busy = computed(() => props.refreshing || isBusy(props.thread.fetchStatus))

const meta = computed(() => {
  const th = props.thread
  const channel = th.channel ? `#${th.channel}` : th.channelId ? `#${th.channelId}` : ''
  return [channel, th.workspace ?? th.workspaceDomain ?? ''].filter(Boolean).join(' · ')
})

const metaTitle = computed(() =>
  props.thread.fetchedAt
    ? t('slackThread.header.fetchedAt', { date: formatFullDate(props.thread.fetchedAt) })
    : t('slackThread.header.notFetched'),
)
</script>

<template>
  <div class="flex shrink-0 items-center gap-3 border-b border-border/60 px-5 py-2.5">
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-1.5">
        <InlineRenameInput
          v-if="renaming"
          class="w-full text-[15px] font-bold leading-snug"
          :value="thread.title"
          :placeholder="t('slackThread.header.renamePlaceholder')"
          @commit="emit('commitRename', $event)"
          @cancel="emit('cancelRename')"
        />
        <template v-else>
          <h1
            class="min-w-0 cursor-text truncate rounded px-0.5 -mx-0.5 text-[15px] font-bold leading-snug transition-colors hover:bg-accent/50"
            :title="`${thread.title}\n${t('slackThread.header.renameHint')}`"
            @click="emit('startRename')"
          >
            {{ thread.title || t('slackThread.untitled') }}
          </h1>
          <span
            v-if="thread.titleCustom"
            class="shrink-0 text-muted-foreground/60"
            :title="t('slackThread.header.renamedHint')"
            :aria-label="t('slackThread.header.renamedHint')"
          >
            <PencilLine class="h-3.5 w-3.5" :stroke-width="1.75" />
          </span>
        </template>
      </div>
      <p v-if="meta" class="mt-0.5 truncate text-xs text-muted-foreground" :title="metaTitle">{{ meta }}</p>
    </div>

    <div class="flex shrink-0 items-center gap-1.5">
      <Button
        :variant="showOriginal ? 'primary' : 'outline'"
        size="xs"
        :aria-pressed="showOriginal"
        @click="emit('toggleOriginal')"
      >
        <Languages class="h-3 w-3" :stroke-width="1.75" />
        {{ t('slackThread.header.showOriginal') }}
      </Button>
      <Button variant="outline" size="xs" :disabled="busy" @click="emit('refresh')">
        <RotateCw class="h-3 w-3" :class="{ 'animate-spin': busy }" :stroke-width="1.75" />
        {{ t('slackThread.header.refresh') }}
      </Button>
      <Button v-if="thread.threadUrl" variant="outline" size="xs" @click="emit('openInSlack')">
        <ExternalLink class="h-3 w-3" :stroke-width="1.75" />
        {{ t('slackThread.header.openInSlack') }}
      </Button>
    </div>
  </div>
</template>
