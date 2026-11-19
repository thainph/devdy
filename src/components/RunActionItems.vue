<script setup lang="ts">
// The actions on one run / session, as DropdownItems — the project History
// row's right-click menu. RunView owns the handlers (and their busy state);
// this only renders and emits.
//
//   section 'actions'  Pop out · Open on GitHub · Refetch | Mark read / unread |
//                      Rename · Pin · Protect | Copy link · Copy session ID ·
//                      Copy log path · View log file
//   section 'delete'   | Stop (running, when `stoppable`) or Delete (not running)
//
// Read state and Stop only show when the caller passes them.
import { useI18n } from 'vue-i18n'
import {
  AppWindow,
  CircleStop,
  ClipboardCopy,
  ExternalLink,
  Eye,
  EyeOff,
  Hash,
  Link,
  Pencil,
  Pin,
  PinOff,
  RefreshCw,
  ScrollText,
  Shield,
  ShieldOff,
  Trash2,
} from 'lucide-vue-next'
import { DropdownItem, DropdownSeparator } from '@/components/ui'
import type { RunRecord } from '@/stores/runs'

withDefaults(defineProps<{
  run: RunRecord
  section: 'actions' | 'delete'
  /** A session pop-out can't pop itself out again. */
  isSessionWindow: boolean
  /** The run's issue / PR on GitHub, when any. */
  githubUrl: string | null
  /** An action for this run is in flight (its item is disabled meanwhile). */
  busy: { refetching: boolean; pinning: boolean; protecting: boolean; deleting: boolean }
  /** Read state to offer toggling; null / omitted = no read items. */
  unread?: boolean | null
  /** Offer "Stop" while the run is running. */
  stoppable?: boolean
}>(), { unread: null, stoppable: false })

const emit = defineEmits<{
  popout: []
  openGithub: []
  refetch: []
  markRead: [read: boolean]
  copyLink: []
  copyLogPath: []
  copyId: []
  viewLog: []
  rename: []
  togglePin: []
  toggleProtect: []
  stop: []
  delete: []
}>()

const { t } = useI18n()
</script>

<template>
  <template v-if="section === 'actions'">
    <!-- Pop this session out into its own OS window (one per run). Hidden inside
         a session window — can't re-pop what's popped. -->
    <DropdownItem v-if="!isSessionWindow" @click="emit('popout')">
      <AppWindow class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.popOutSession') }}
    </DropdownItem>
    <DropdownItem v-if="githubUrl" @click="emit('openGithub')">
      <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ run.run_type === 'analyze_issue' ? t('run.openIssueOnGithub') : t('run.openPrOnGithub') }}
    </DropdownItem>
    <DropdownItem
      v-if="run.run_type !== 'session' && run.status !== 'running'"
      :disabled="busy.refetching"
      @click="emit('refetch')"
    >
      <RefreshCw class="h-3.5 w-3.5 shrink-0" :class="{ 'animate-spin': busy.refetching }" :stroke-width="1.75" />
      {{ run.run_type === 'analyze_issue' ? t('run.refetchIssue') : t('run.refetchPr') }}
    </DropdownItem>
    <template v-if="unread !== null && run.status !== 'running'">
      <DropdownSeparator />
      <DropdownItem v-if="unread" @click="emit('markRead', true)">
        <Eye class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('dashboard.history.menu.markRead') }}
      </DropdownItem>
      <DropdownItem v-else @click="emit('markRead', false)">
        <EyeOff class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('dashboard.history.menu.markUnread') }}
      </DropdownItem>
    </template>
    <DropdownSeparator />
    <DropdownItem @click="emit('rename')">
      <Pencil class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.rename') }}
    </DropdownItem>
    <DropdownItem :disabled="busy.pinning" @click="emit('togglePin')">
      <component :is="run.pinned ? PinOff : Pin" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ run.pinned ? t('run.unpinFromTop') : t('run.pinToTop') }}
    </DropdownItem>
    <DropdownItem :disabled="busy.protecting" @click="emit('toggleProtect')">
      <component :is="run.protected ? ShieldOff : Shield" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ run.protected ? t('run.unprotect') : t('run.protect') }}
    </DropdownItem>
    <DropdownSeparator />
    <DropdownItem v-if="githubUrl" @click="emit('copyLink')">
      <Link class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ run.run_type === 'analyze_issue' ? t('dashboard.history.menu.copyIssueLink') : t('dashboard.history.menu.copyPrLink') }}
    </DropdownItem>
    <DropdownItem @click="emit('copyId')">
      <Hash class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.copySessionId') }}
    </DropdownItem>
    <DropdownItem @click="emit('copyLogPath')">
      <ClipboardCopy class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.copyLogPath') }}
    </DropdownItem>
    <DropdownItem @click="emit('viewLog')">
      <ScrollText class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.viewLogFile') }}
    </DropdownItem>
  </template>

  <template v-else-if="run.status === 'running'">
    <template v-if="stoppable">
      <DropdownSeparator />
      <DropdownItem variant="destructive" @click="emit('stop')">
        <CircleStop class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('dashboard.history.menu.stop') }}…
      </DropdownItem>
    </template>
  </template>

  <template v-else>
    <DropdownSeparator />
    <DropdownItem variant="destructive" :disabled="busy.deleting" @click="emit('delete')">
      <Trash2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.deleteThisRun') }}
    </DropdownItem>
  </template>
</template>
