<script setup lang="ts">
// Header of the thread pane: the title (click → inline rename) over
// `#channel · workspace`; on the right the thread's project + its "Sessions"
// menu (linked sessions, link one — LinkedSessionsMenu), both bordered xs
// controls, then the icon-only thread actions (label in tooltip + aria-label) —
// whole-thread "Show original" toggle (primary while on), Refresh (spinner +
// disabled while a job runs), Open in Slack and the ⋯ menu (New session from
// this thread — disabled without a project · Copy ID · Re-translate — disabled
// while a job runs · Delete). "Watch" toggles the thread in Project Pulse
// (asks for a project first when the thread has none). The host
// confirms deletes and shows the toasts.
//
// Narrow pane: the title gives way first (down to 6rem), then the project name
// truncates.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ExternalLink, Languages, PencilLine, RefreshCcw, RotateCw } from 'lucide-vue-next'
import { Button, DropdownItem } from '@/components/ui'
import InlineRenameInput from '@/components/InlineRenameInput.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { formatFullDate } from '@/lib/slackThreadUtils'
import { isBusy, type SlackThreadRunLink, type SlackThreadSummary } from '@/stores/slackThreads'
import LinkedSessionsMenu from '@/components/sessions/LinkedSessionsMenu.vue'
import DetailActionsMenu from '@/components/sessions/DetailActionsMenu.vue'
import PulseWatchButton from '@/components/pulse/PulseWatchButton.vue'
import { slackSourceRef } from '@/lib/pulse'
import type { SessionLink } from '@/lib/sessionLinks'

const props = defineProps<{
  thread: SlackThreadSummary
  /** Sessions linked to the thread. */
  links: SlackThreadRunLink[]
  renaming: boolean
  showOriginal: boolean
  refreshing: boolean
  /** Copy ID just fired: its icon becomes a tick. */
  copiedId: boolean
}>()

const emit = defineEmits<{
  startRename: []
  commitRename: [title: string]
  cancelRename: []
  toggleOriginal: []
  refresh: []
  retranslate: []
  openInSlack: []
  setProject: [projectId: string | null]
  openRun: [link: SessionLink]
  unlinkRun: [link: SessionLink]
  linkRun: [runId: string]
  newSession: []
  copyId: []
  delete: []
}>()

const { t } = useI18n()

const busy = computed(() => props.refreshing || isBusy(props.thread.fetchStatus))

const meta = computed(() => {
  const th = props.thread
  const channel = th.channel ? `#${th.channel}` : th.channelId ? `#${th.channelId}` : ''
  return [channel, th.workspace ?? th.workspaceDomain ?? ''].filter(Boolean).join(' · ')
})

const pulseSource = computed(() => ({
  type: 'slack' as const,
  ref: slackSourceRef(props.thread.channelId, props.thread.threadTs),
  url: props.thread.threadUrl,
}))

const metaTitle = computed(() =>
  props.thread.fetchedAt
    ? t('slackThread.header.fetchedAt', { date: formatFullDate(props.thread.fetchedAt) })
    : t('slackThread.header.notFetched'),
)
</script>

<template>
  <div class="flex shrink-0 items-center gap-3 border-b border-border/60 px-5 py-2.5">
    <div class="min-w-[6rem] flex-1">
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

    <div class="flex min-w-0 items-center gap-1.5">
      <!-- The thread's project and its sessions: linking depends on the project. -->
      <div class="w-[11rem] min-w-[5.5rem] shrink">
        <ProjectSelect
          :model-value="thread.projectId"
          variant="outline"
          size="xs"
          @update:model-value="emit('setProject', $event)"
        />
      </div>
      <LinkedSessionsMenu
        class="shrink-0"
        :project-id="thread.projectId"
        :links="links"
        @open-run="emit('openRun', $event)"
        @unlink-run="emit('unlinkRun', $event)"
        @link="emit('linkRun', $event)"
      />
      <PulseWatchButton
        v-if="!thread.legacy && pulseSource.ref"
        :source="pulseSource"
        :project-id="thread.projectId"
        variant="label"
        @project-chosen="emit('setProject', $event)"
      />
      <div class="mx-0.5 h-4 w-px shrink-0 bg-border" aria-hidden="true" />
      <Button
        :variant="showOriginal ? 'primary' : 'outline'"
        size="icon-sm"
        class="shrink-0"
        :aria-pressed="showOriginal"
        :title="t('slackThread.header.showOriginal')"
        :aria-label="t('slackThread.header.showOriginal')"
        @click="emit('toggleOriginal')"
      >
        <Languages class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        variant="outline"
        size="icon-sm"
        class="shrink-0"
        :disabled="busy"
        :title="t('slackThread.header.refresh')"
        :aria-label="t('slackThread.header.refresh')"
        @click="emit('refresh')"
      >
        <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': busy }" :stroke-width="1.75" />
      </Button>
      <Button
        v-if="thread.threadUrl"
        variant="outline"
        size="icon-sm"
        class="shrink-0"
        :title="t('slackThread.header.openInSlack')"
        :aria-label="t('slackThread.header.openInSlack')"
        @click="emit('openInSlack')"
      >
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <DetailActionsMenu
        :new-session-label="t('slackThread.links.newSession')"
        :new-session-hint="thread.projectId ? t('slackThread.links.newSessionHint') : t('slackThread.links.needProject')"
        :can-new-session="!!thread.projectId"
        :copied-id="copiedId"
        @new-session="emit('newSession')"
        @copy-id="emit('copyId')"
        @delete="emit('delete')"
      >
        <DropdownItem
          :disabled="busy"
          :title="t('slackThread.header.retranslateHint')"
          @click="emit('retranslate')"
        >
          <RefreshCcw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('slackThread.header.retranslate') }}
        </DropdownItem>
      </DetailActionsMenu>
    </div>
  </div>
</template>
