<script setup lang="ts">
// The actions on one run / session, as DropdownItems — shared by the History
// row's ⋯ menu and the RunView header's ⋯ menu, so both offer the same list with
// the same rules. RunView owns the handlers (and their busy state); this only
// renders and emits.
//
//   section 'actions'  Pop out · Open on GitHub · Refetch · Copy log path ·
//                      Copy session ID · View log file | Rename · Pin · Protect ·
//                      Convert to conductor
//   section 'delete'   | Delete (hidden while the run is running)
import { useI18n } from 'vue-i18n'
import {
  AppWindow,
  ClipboardCopy,
  ExternalLink,
  Hash,
  Network,
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

defineProps<{
  run: RunRecord
  section: 'actions' | 'delete'
  /** A session pop-out can't pop itself out again. */
  isSessionWindow: boolean
  /** The run's issue / PR on GitHub, when any. */
  githubUrl: string | null
  /** An action for this run is in flight (its item is disabled meanwhile). */
  busy: { refetching: boolean; pinning: boolean; protecting: boolean; converting: boolean; deleting: boolean }
}>()

const emit = defineEmits<{
  popout: []
  openGithub: []
  refetch: []
  copyLogPath: []
  copyId: []
  viewLog: []
  rename: []
  togglePin: []
  toggleProtect: []
  convert: []
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
    <DropdownItem @click="emit('copyLogPath')">
      <ClipboardCopy class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.copyLogPath') }}
    </DropdownItem>
    <DropdownItem @click="emit('copyId')">
      <Hash class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.copySessionId') }}
    </DropdownItem>
    <DropdownItem @click="emit('viewLog')">
      <ScrollText class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.viewLogFile') }}
    </DropdownItem>
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
    <DropdownItem
      v-if="run.run_type === 'session' && run.role !== 'conductor' && run.role !== 'worker' && run.status !== 'running' && run.status !== 'fetched'"
      :disabled="busy.converting"
      @click="emit('convert')"
    >
      <Network class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.convertToConductor') }}
    </DropdownItem>
  </template>

  <template v-else-if="run.status !== 'running'">
    <DropdownSeparator />
    <DropdownItem variant="destructive" :disabled="busy.deleting" @click="emit('delete')">
      <Trash2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('run.deleteThisRun') }}
    </DropdownItem>
  </template>
</template>
