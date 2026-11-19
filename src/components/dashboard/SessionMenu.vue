<script lang="ts">
/** What the menu acts on: a History row or a running session. */
export interface SessionMenuTarget {
  runId: string
  projectId: string
  /** Display name (for the confirmations). */
  name: string
  /** `session` | `analyze_issue` | `review_pr`. */
  runType: string
  refNumber: number | null
  repoPath: string | null
  repoProvider: string | null
  /** Its current title, the rename prompt's starting value. */
  title: string | null
  pinned: boolean
  protected: boolean
  /** Its sidecar is alive: "Stop" is offered. */
  live: boolean
  /** It can be deleted (a stopped History row). */
  deletable: boolean
  /** Read state; null = none (a running session). */
  unread: boolean | null
  /** Only the "open" items (a session outside Devdy mirrored into a run). */
  openOnly?: boolean
}

/** Where to anchor the menu: the pointer, or (Menu key / Shift+F10 fire at 0,0) the row. */
export function menuPoint(e: MouseEvent): { x: number; y: number } {
  if ((e.clientX || e.clientY) || !(e.currentTarget instanceof HTMLElement)) return { x: e.clientX, y: e.clientY }
  const box = e.currentTarget.getBoundingClientRect()
  return { x: box.left + 24, y: box.bottom }
}
</script>

<script setup lang="ts">
// The right-click menu of a session on the Dashboard — shared by History rows
// and running rows, so both offer the same actions with the same rules: open
// here or in the main window, the issue / PR on GitHub / GitLab, read state
// (History only), rename / pin / protect, copy the link / id, and stop (live)
// or delete (stopped History row). Items that don't apply are hidden.
//
// The parent opens the run (it owns where it goes) and refreshes its list on
// `changed`; everything else happens here.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  AppWindow,
  CircleDot,
  CircleStop,
  ExternalLink,
  Eye,
  EyeOff,
  GitPullRequest,
  Hash,
  Link,
  Pencil,
  Pin,
  PinOff,
  Shield,
  ShieldOff,
  Trash2,
} from 'lucide-vue-next'
import { ContextMenu, DropdownItem, DropdownSeparator } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import { historyKind, historyWebUrl } from '@/lib/runHistory'
import { useConfirm } from '@/composables/useConfirm'
import { usePrompt } from '@/composables/usePrompt'
import { useToast } from '@/composables/useToast'
import { useRunsStore } from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'

const props = defineProps<{ open: boolean; x: number; y: number; target: SessionMenuTarget | null }>()
const emit = defineEmits<{
  close: []
  /** Open the run in its own window or in the main window. */
  openRun: [target: SessionMenuTarget, where: 'window' | 'main']
  /** Marked read (false) or unread (true). */
  read: [target: SessionMenuTarget, unread: boolean]
  /** Renamed / pinned / protected / stopped: re-read the list. */
  changed: []
  deleted: [runId: string]
}>()

const { t } = useI18n()
const { confirm } = useConfirm()
const { prompt } = usePrompt()
const { toast } = useToast()
const runsStore = useRunsStore()
const live = useLiveRunsStore()
const tabs = useWorkspaceTabsStore()

const webUrl = computed(() => (props.target ? historyWebUrl(props.target) : null))
const isIssue = computed(() => (props.target ? historyKind(props.target) === 'Issue' : false))
const webLabel = computed(() => {
  const host = props.target?.repoProvider === 'gitlab' ? 'Gitlab' : 'Github'
  return t(`dashboard.history.menu.${isIssue.value ? 'issue' : 'pr'}On${host}`)
})

/** Run an action on the menu's target, closing the menu first. */
function act(fn: (s: SessionMenuTarget) => unknown) {
  const s = props.target
  emit('close')
  if (!s) return
  Promise.resolve(fn(s)).catch((e) => toast.error(String(e)))
}

async function setRead(s: SessionMenuTarget, read: boolean) {
  emit('read', s, !read)
  if (read) {
    live.markSeen(s.runId)
    await invoke('mark_run_seen', { runId: s.runId })
  } else {
    await invoke('mark_run_unread', { runId: s.runId })
  }
}
async function rename(s: SessionMenuTarget) {
  const title = await prompt({ title: t('run.rename'), label: t('run.runNamePlaceholder'), initialValue: s.title ?? '' })
  if (title === null) return
  await runsStore.renameRun(s.runId, title)
  emit('changed')
}
async function togglePin(s: SessionMenuTarget) {
  await runsStore.setRunPinned(s.runId, !s.pinned)
  emit('changed')
}
async function toggleProtect(s: SessionMenuTarget) {
  await runsStore.setRunProtected(s.runId, !s.protected)
  toast.success(s.protected ? t('run.runUnprotected') : t('run.runProtected'))
  emit('changed')
}
async function copy(text: string, done: string) {
  await navigator.clipboard.writeText(text)
  toast.success(done)
}
async function stop(s: SessionMenuTarget) {
  const ok = await confirm({
    title: t('dashboard.history.menu.stopTitle'),
    message: t('dashboard.history.menu.stopMessage', { name: s.name }),
    confirmLabel: t('dashboard.history.menu.stop'),
  })
  if (!ok) return
  await runsStore.cancelRun(s.runId)
  emit('changed')
}
async function remove(s: SessionMenuTarget) {
  const ok = await confirm({
    title: t('run.deleteRunTitle'),
    message: t('run.deleteRunMessage', { label: s.name }),
    confirmLabel: t('common.delete'),
  })
  if (!ok) return
  const leftover = await runsStore.deleteRun(s.runId)
  live.discard(s.runId)
  tabs.forgetRun(s.runId)
  emit('deleted', s.runId)
  if (leftover.length) toast.error(t('run.runDeletedWithLeftovers', { count: leftover.length, paths: leftover.join('\n') }))
  else toast.success(t('run.runDeleted'))
}
</script>

<template>
  <ContextMenu :open="open" :x="x" :y="y" @close="emit('close')">
    <template v-if="target">
      <DropdownItem class="font-semibold" @click="act((s) => emit('openRun', s, 'window'))">
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('dashboard.history.menu.openWindow') }}
      </DropdownItem>
      <DropdownItem @click="act((s) => emit('openRun', s, 'main'))">
        <AppWindow class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('dashboard.history.menu.openMain') }}
      </DropdownItem>
      <template v-if="!target.openOnly">
        <DropdownItem v-if="webUrl" @click="act(() => openUrl(webUrl ?? ''))">
          <CircleDot v-if="isIssue" class="h-3.5 w-3.5" :stroke-width="1.75" />
          <GitPullRequest v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ webLabel }}
        </DropdownItem>
        <template v-if="target.unread !== null">
          <DropdownSeparator />
          <DropdownItem v-if="target.unread" @click="act((s) => setRead(s, true))">
            <Eye class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('dashboard.history.menu.markRead') }}
          </DropdownItem>
          <DropdownItem v-else @click="act((s) => setRead(s, false))">
            <EyeOff class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('dashboard.history.menu.markUnread') }}
          </DropdownItem>
        </template>
        <DropdownSeparator />
        <DropdownItem @click="act(rename)">
          <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('run.rename') }}
        </DropdownItem>
        <DropdownItem @click="act(togglePin)">
          <component :is="target.pinned ? PinOff : Pin" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ target.pinned ? t('run.unpinFromTop') : t('run.pinToTop') }}
        </DropdownItem>
        <DropdownItem @click="act(toggleProtect)">
          <component :is="target.protected ? ShieldOff : Shield" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ target.protected ? t('run.unprotect') : t('run.protect') }}
        </DropdownItem>
        <DropdownSeparator />
        <DropdownItem v-if="webUrl" @click="act(() => copy(webUrl ?? '', t('dashboard.history.menu.linkCopied')))">
          <Link class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ isIssue ? t('dashboard.history.menu.copyIssueLink') : t('dashboard.history.menu.copyPrLink') }}
        </DropdownItem>
        <DropdownItem @click="act((s) => copy(s.runId, t('run.sessionIdCopied')))">
          <Hash class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('run.copySessionId') }}
        </DropdownItem>
        <template v-if="target.live || target.deletable">
          <DropdownSeparator />
          <DropdownItem v-if="target.live" variant="destructive" @click="act(stop)">
            <CircleStop class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('dashboard.history.menu.stop') }}…
          </DropdownItem>
          <DropdownItem v-else variant="destructive" @click="act(remove)">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('dashboard.history.menu.delete') }}…
          </DropdownItem>
        </template>
      </template>
    </template>
  </ContextMenu>
</template>
