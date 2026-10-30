<script setup lang="ts">
// Settings → Sync: on-demand sync of the Devdy state between this user's machines over the
// relay (E2E encrypted). Not in a group → create one, or join another device's group with its
// device name + master password (that device must have "Accept a new device" open and approve the
// request; both need their own relay config). Joined but not merged (`sync_bootstrap_pending`, also after an
// app restart) → the merge step: preview (totals, per table, project matches) then commit, which
// starts the engine. In a group → four tabs:
// - Overview: source device (+ online), "Sync from <device>" (opens the Sync dialog, optionally with
//   project folders), last sync, changes waiting to be sent, pause, changes the relay refused
//   (parked, with Retry), "merge again" when the relay lost ops (`relay_behind`), re-sync, leave.
// - Data: categories this machine receives (SyncDataTab). - Projects: per-project selection,
//   folder mapping and folder sync (SyncProjectsTab).
// - Devices: devices (rename this one, remove others — creator only —, pick the leader that runs
//   Pulse / Slack polling) and "Accept a new device" (5 min window + Allow / Deny per request,
//   closed when this page goes away so a request is never left unseen).
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  AlertTriangle, ChevronRight, Crown, Loader2, LogIn, LogOut, Pause, Pencil, Play, Plus, RefreshCw, RotateCcw, Trash2,
  UserPlus, Users,
} from 'lucide-vue-next'
import { AppSelect, Badge, Button, Card, Input } from '@/components/ui'
import SyncDataTab from '@/components/SyncDataTab.vue'
import SyncProjectsTab from '@/components/SyncProjectsTab.vue'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useSyncStore } from '@/stores/sync'
import {
  ageParts,
  canRemoveDevices,
  countdown,
  deviceLabel,
  groupCreator,
  onSyncAcceptWindow,
  onSyncJoinRequest,
  onSyncJoinResolved,
  syncAcceptClose,
  syncAcceptOpen,
  syncAcceptState,
  syncJoinRespond,
  syncRelayReady,
  missingProjects,
  syncBootstrapCommit,
  syncBootstrapPreview,
  syncCreateGroup,
  syncDeviceRename,
  syncJoin,
  syncLeave,
  syncParkedList,
  syncParkedRetry,
  syncPause,
  syncRebootstrap,
  syncResyncAll,
  syncRevokeDevice,
  syncSetLeader,
  SYNC_DOT_CLASS,
  folderProjects,
  syncErrorCode,
  syncStateKey,
  syncTone,
  tableRows,
  totalRows,
  type BootstrapPreview,
  type BootstrapReport,
  type JoinRequest,
  type ParkedRow,
  type SyncDevice,
} from '@/lib/sync'
import { DEVICE_ALIAS_RE } from '@/lib/remoteLogin'

const { t, te } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const store = useSyncStore()

const status = computed(() => store.status)
const paired = computed(() => status.value.state !== 'disabled')
const mergePending = computed(() => store.mergePending)
const tone = computed(() => syncTone(status.value, mergePending.value))

// Relative times ("3 min ago") re-render on this clock.
const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null

function ago(ms: number | null | undefined): string {
  const { unit, n } = ageParts(ms, now.value)
  return t(`settings.sync.ago.${unit}`, { n })
}

/** Backend errors with a known code get a translated message. */
function errorText(e: unknown): string {
  const code = syncErrorCode(e)
  return code ? t(`settings.sync.errors.${code}`) : String(e)
}

// ---- Not in a group: create / join ----
/** Own relay URL + token configured (Remote Control); null = not checked yet. */
const relayReady = ref<boolean | null>(null)
const creating = ref(false)
const joinAlias = ref('')
const joinPassword = ref('')
const joining = ref(false)
/** Joined, not merged: the first-join merge preview waiting for "Commit". */
const preview = ref<BootstrapPreview | null>(null)
const previewLoading = ref(false)
const previewError = ref('')
const committing = ref(false)
const commitReport = ref<BootstrapReport | null>(null)
const previewTables = computed(() => (preview.value ? tableRows(preview.value) : []))
const previewMissing = computed(() => (preview.value ? missingProjects(preview.value) : []))

function countLabel([key, n]: [string, number]): string {
  return `${t(`settings.sync.join.counts.${key}`)}: ${n}`
}

const joinError = computed(() => {
  const alias = joinAlias.value.trim().toLowerCase()
  if (alias && !DEVICE_ALIAS_RE.test(alias)) return t('settings.sync.join.aliasInvalid')
  return ''
})

async function createGroup() {
  if (creating.value) return
  creating.value = true
  try {
    store.set(await syncCreateGroup())
    toast.success(t('settings.sync.create.done'))
    await loadPaired()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    creating.value = false
  }
}

async function join() {
  const alias = joinAlias.value.trim().toLowerCase()
  if (joining.value || !alias || !joinPassword.value || joinError.value) return
  joining.value = true
  try {
    store.set(await syncJoin(alias, joinPassword.value))
    joinPassword.value = ''
    store.setMergePending(true)
    await loadPreview()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    joining.value = false
  }
}

async function loadPreview() {
  if (previewLoading.value) return
  previewLoading.value = true
  previewError.value = ''
  try {
    preview.value = await syncBootstrapPreview()
  } catch (e) {
    preview.value = null
    previewError.value = errorText(e)
  } finally {
    previewLoading.value = false
  }
}

async function commitBootstrap() {
  if (committing.value) return
  committing.value = true
  try {
    commitReport.value = await syncBootstrapCommit()
    preview.value = null
    store.setMergePending(false)
    await store.refresh()
    toast.success(t('settings.sync.join.done'))
    await loadPaired()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    committing.value = false
  }
}

// ---- In a group ----
type Tab = 'overview' | 'data' | 'projects' | 'devices'
const TABS: Tab[] = ['overview', 'data', 'projects', 'devices']
const tab = ref<Tab>('overview')

const busyPause = ref(false)

const devices = computed(() => store.devices)
const renaming = ref(false)
const newName = ref('')
const busyDevice = ref<string | null>(null)

// ---- Overview: source device + Sync ----
const otherDevices = computed(() => devices.value.filter((d) => !d.is_self))
const sourceOptions = computed(() => otherDevices.value.map((d) => ({ value: d.id, label: deviceLabel(d) })))
const source = computed(() => store.source)
const sourceName = computed(() => store.deviceName(source.value))
const sourceOnline = computed(() => store.isOnline(source.value))
const folderCount = computed(() => folderProjects(store.prefs?.projects ?? []).length)
const lastRun = computed(() => store.lastRun)
let peersTimer: ReturnType<typeof setInterval> | null = null

async function changeSource(id: string) {
  if (!id || id === source.value) return
  try {
    await store.setSource(id)
  } catch (e) {
    toast.error(errorText(e))
  }
}

function syncFromSource() {
  void store.syncFrom(source.value, store.includeFolders)
}

const parked = ref<ParkedRow[]>([])
const retrying = ref(false)

const resyncing = ref(false)
const leaving = ref(false)

const self = computed(() => devices.value.find((d) => d.is_self) ?? null)
const leaderId = computed(() => devices.value.find((d) => d.is_leader)?.id ?? null)
const canRemove = computed(() => canRemoveDevices(devices.value))
const creator = computed(() => groupCreator(devices.value))

// ---- Accept a new device (host side of a join) ----
/** Expiry (unix millis) of the open window, null = closed. */
const acceptUntil = ref<number | null>(null)
const acceptBusy = ref(false)
const joinRequest = ref<JoinRequest | null>(null)
const answering = ref(false)
// The countdowns tick every second while something is open.
const tick = ref(Date.now())
let ticker: ReturnType<typeof setInterval> | null = null
const unlisteners: UnlistenFn[] = []

function setAcceptUntil(v: number | null) {
  acceptUntil.value = v
  if (v === null) joinRequest.value = null
  if (v !== null && !ticker) {
    ticker = setInterval(() => {
      tick.value = Date.now()
      if (acceptUntil.value !== null && tick.value >= acceptUntil.value) setAcceptUntil(null)
    }, 1000)
  } else if (v === null && ticker) {
    clearInterval(ticker)
    ticker = null
  }
  tick.value = Date.now()
}

async function openAccept() {
  if (acceptBusy.value) return
  acceptBusy.value = true
  try {
    setAcceptUntil(await syncAcceptOpen())
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    acceptBusy.value = false
  }
}

async function closeAccept() {
  setAcceptUntil(null)
  await syncAcceptClose().catch(() => {})
}

async function answer(allow: boolean) {
  const req = joinRequest.value
  if (!req || answering.value) return
  answering.value = true
  try {
    await syncJoinRespond(req.request_id, allow)
    if (!allow) toast.info(t('settings.sync.accept.denied'))
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    answering.value = false
    if (joinRequest.value?.request_id === req.request_id) joinRequest.value = null
  }
}

async function loadDevices() {
  try {
    await store.loadDevices()
  } catch (e) {
    store.devices = []
    toast.error(errorText(e))
  }
}

async function loadParked() {
  parked.value = await syncParkedList().catch(() => [])
}

async function retryParked() {
  if (retrying.value) return
  retrying.value = true
  try {
    const n = await syncParkedRetry()
    toast.success(t('settings.sync.parked.retried', { n }))
    await loadParked()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    retrying.value = false
  }
}

function parkedReason(reason: string): string {
  const key = `settings.sync.parked.reason.${reason}`
  return te(key) ? t(key) : reason
}

async function loadPaired() {
  if (!paired.value) return
  await Promise.all([loadDevices(), loadParked(), store.loadPrefs().catch(() => {}), store.refreshPeers()])
}

async function togglePause() {
  if (busyPause.value) return
  busyPause.value = true
  try {
    store.set(await syncPause(!status.value.paused))
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    busyPause.value = false
  }
}

function startRename() {
  newName.value = self.value?.name ?? ''
  renaming.value = true
}

async function saveRename() {
  const name = newName.value.trim()
  if (!name) return
  busyDevice.value = self.value?.id ?? 'self'
  try {
    await syncDeviceRename(name)
    renaming.value = false
    await loadDevices()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    busyDevice.value = null
  }
}

async function removeDevice(d: SyncDevice) {
  const ok = await confirm({
    title: t('settings.sync.devices.confirmRemove.title'),
    message: t('settings.sync.devices.confirmRemove.message', { name: deviceLabel(d) }),
    confirmLabel: t('settings.sync.devices.confirmRemove.confirmLabel'),
    variant: 'destructive',
  })
  if (!ok) return
  busyDevice.value = d.id
  try {
    await syncRevokeDevice(d.id)
    toast.success(t('settings.sync.devices.removed', { name: deviceLabel(d) }))
    await loadDevices()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    busyDevice.value = null
  }
}

async function setLeader(id: string | null) {
  if (id === leaderId.value) return
  busyDevice.value = id ?? 'none'
  try {
    const confirmed = await syncSetLeader(id)
    await loadDevices()
    if (!confirmed) {
      const next = devices.value.find((d) => d.id === id)
      toast.info(t('settings.sync.devices.leaderPending', { name: next ? deviceLabel(next) : id ?? '' }))
    }
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    busyDevice.value = null
  }
}

async function resyncAll() {
  const ok = await confirm({
    title: t('settings.sync.resync.confirm.title'),
    message: t('settings.sync.resync.confirm.message'),
    confirmLabel: t('settings.sync.resync.confirm.confirmLabel'),
  })
  if (!ok) return
  resyncing.value = true
  try {
    const n = await syncResyncAll()
    toast.success(t('settings.sync.resync.done', { n }))
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    resyncing.value = false
  }
}

// The relay lost ops this machine already pulled: sync stays stopped until it merges again.
const relayBehind = computed(() => syncErrorCode(status.value.last_error) === 'relay_behind')
const rebootstrapping = ref(false)

async function rebootstrap() {
  if (rebootstrapping.value) return
  rebootstrapping.value = true
  try {
    await syncRebootstrap()
    await store.refresh()
    store.setMergePending(true)
    await loadPreview()
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    rebootstrapping.value = false
  }
}

async function leave() {
  const ok = await confirm({
    title: t('settings.sync.leave.confirm.title'),
    message: t('settings.sync.leave.confirm.message'),
    confirmLabel: t('settings.sync.leave.confirm.confirmLabel'),
    variant: 'destructive',
  })
  if (!ok) return
  leaving.value = true
  try {
    setAcceptUntil(null)
    const revoked = await syncLeave().catch(async (e: unknown) => {
      // Leader / creator offline: its role hand-off would be lost; leave only if confirmed.
      if (syncErrorCode(e) !== 'sync_leave_handoff_pending') throw e
      const anyway = await confirm({
        title: t('settings.sync.leave.handoff.title'),
        message: t('settings.sync.leave.handoff.message'),
        confirmLabel: t('settings.sync.leave.handoff.confirmLabel'),
        variant: 'destructive',
      })
      return anyway ? syncLeave(true) : null
    })
    if (revoked === null) return
    await store.refresh()
    store.devices = []
    store.prefs = null
    parked.value = []
    preview.value = null
    previewError.value = ''
    commitReport.value = null
    if (revoked) toast.success(t('settings.sync.leave.done'))
    else toast.info(t('settings.sync.leave.offline'))
    relayReady.value = await syncRelayReady().catch(() => false)
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    leaving.value = false
  }
}

// The engine re-counts parked rows every cycle: keep the list in step.
watch(
  () => status.value.parked,
  (n, old) => {
    if (paired.value && n !== old) void loadParked()
  },
)

onMounted(async () => {
  clock = setInterval(() => (now.value = Date.now()), 30_000)
  // Who is online, for the source dot (only while in a group).
  peersTimer = setInterval(() => {
    if (paired.value && !mergePending.value) void store.refreshPeers()
  }, 20_000)
  unlisteners.push(
    await onSyncJoinRequest((r) => {
      joinRequest.value = r
      tick.value = Date.now()
    }),
    await onSyncJoinResolved((id, granted) => {
      const req = joinRequest.value
      if (req?.request_id === id) joinRequest.value = null
      if (granted) toast.success(t('settings.sync.accept.allowed', { name: req?.device_name ?? '' }))
    }),
    await onSyncAcceptWindow(setAcceptUntil),
  )
  await store.start()
  await store.refresh()
  relayReady.value = await syncRelayReady().catch(() => false)
  if (mergePending.value) await loadPreview()
  else await loadPaired()
  if (paired.value) setAcceptUntil(await syncAcceptState().catch(() => null))
})

onUnmounted(() => {
  if (clock) clearInterval(clock)
  if (peersTimer) clearInterval(peersTimer)
  for (const u of unlisteners) u()
  // Nobody would see a join request any more: stop accepting.
  if (acceptUntil.value !== null) void syncAcceptClose().catch(() => {})
  setAcceptUntil(null)
})
</script>

<template>
  <Card body-class="p-4 space-y-5">
    <template #header>
      <RefreshCw class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('settings.sync.title') }}</span>
      <span class="ml-auto flex items-center gap-1.5 text-[11px] text-muted-foreground">
        <span class="h-2 w-2 rounded-full" :class="SYNC_DOT_CLASS[tone]" />
        {{ t(`settings.sync.state.${syncStateKey(status, mergePending)}`) }}
      </span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.help') }}</p>

    <!-- Joined, first-join merge not committed yet -->
    <section v-if="mergePending" class="space-y-3 rounded-lg border border-border/60 bg-muted/20 p-3">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
        {{ t('settings.sync.join.previewTitle') }}
      </label>
      <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.join.pendingHelp') }}</p>
      <p v-if="previewLoading" class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
        <Loader2 class="h-3 w-3 animate-spin" :stroke-width="1.75" />
        {{ t('settings.sync.join.previewLoading') }}
      </p>
      <template v-else-if="preview">
        <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.join.previewHelp') }}</p>
        <ul v-if="totalRows(preview).length" class="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
          <li v-for="[key, n] in totalRows(preview)" :key="key" class="flex justify-between gap-2">
            <span class="truncate text-muted-foreground">{{ t(`settings.sync.join.counts.${key}`) }}</span>
            <span class="tabular-nums">{{ n }}</span>
          </li>
        </ul>
        <p v-else class="text-[11px] text-muted-foreground">{{ t('settings.sync.join.previewEmpty') }}</p>
        <details v-if="previewTables.length" class="text-xs">
          <summary class="cursor-pointer text-[11px] text-muted-foreground">{{ t('settings.sync.join.tablesTitle') }}</summary>
          <ul class="mt-1 space-y-0.5">
            <li v-for="row in previewTables" :key="row.tbl" class="flex justify-between gap-3">
              <span class="truncate font-mono text-muted-foreground">{{ row.tbl }}</span>
              <span class="truncate text-right text-[11px]">{{ row.counts.map(countLabel).join(' · ') }}</span>
            </li>
          </ul>
        </details>
        <div v-if="preview.projects.length" class="space-y-1">
          <p class="text-[11px] font-medium text-muted-foreground">{{ t('settings.sync.join.projectsTitle') }}</p>
          <ul class="divide-y divide-border/60 rounded-md border border-border/60">
            <li v-for="m in preview.projects" :key="m.remote_id" class="px-2.5 py-1.5">
              <p class="flex items-center gap-1.5 truncate text-xs">
                {{ m.name }}
                <Badge variant="secondary">{{ t(`settings.sync.join.matchedBy.${m.matched_by ?? 'new'}`) }}</Badge>
              </p>
              <p
                class="truncate font-mono text-[11px]"
                :class="m.on_this_machine ? 'text-muted-foreground' : 'text-amber-600 dark:text-amber-400'"
                :title="m.local_path"
              >
                {{ m.on_this_machine ? m.local_path : t('settings.sync.projects.notHere') }}
              </p>
            </li>
          </ul>
          <p v-if="previewMissing.length" class="text-[11px] text-muted-foreground">
            {{ t('settings.sync.join.missingHelp', { n: previewMissing.length }) }}
          </p>
        </div>
        <p v-if="preview.undecryptable" class="text-[11px] text-amber-600 dark:text-amber-400">
          {{ t('settings.sync.join.undecryptable', { n: preview.undecryptable }) }}
        </p>
      </template>
      <p v-else-if="previewError" class="break-words text-[11px] text-destructive">
        {{ t('settings.sync.join.previewFailed') }}: {{ previewError }}
      </p>
      <div class="flex flex-wrap items-center gap-2">
        <Button size="sm" :disabled="committing || previewLoading || !preview" @click="commitBootstrap">
          <component :is="committing ? Loader2 : LogIn" class="h-3.5 w-3.5" :class="{ 'animate-spin': committing }" :stroke-width="1.75" />
          {{ t('settings.sync.join.commit') }}
        </Button>
        <Button v-if="previewError && !previewLoading" variant="outline" size="sm" @click="loadPreview">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('settings.sync.join.retry') }}
        </Button>
        <Button variant="destructive" size="sm" class="ml-auto" :disabled="leaving || committing" @click="leave">
          <component :is="leaving ? Loader2 : LogOut" class="h-3.5 w-3.5" :class="{ 'animate-spin': leaving }" :stroke-width="1.75" />
          {{ t('settings.sync.leave.button') }}
        </Button>
      </div>
    </section>

    <!-- Not in a group -->
    <template v-else-if="!paired">
      <section class="space-y-3">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.sync.create.title') }}
        </label>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.create.help') }}</p>
        <p v-if="relayReady === false" class="text-[11px] text-amber-600 dark:text-amber-400">
          {{ t('settings.sync.relayMissing') }}
        </p>
        <Button size="sm" :disabled="creating || relayReady === false" @click="createGroup">
          <component :is="creating ? Loader2 : Plus" class="h-3.5 w-3.5" :class="{ 'animate-spin': creating }" :stroke-width="1.75" />
          {{ t('settings.sync.create.button') }}
        </Button>
      </section>

      <div class="border-t border-border/60" />

      <section class="space-y-3">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.sync.join.title') }}
        </label>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.join.help') }}</p>
        <Input v-model="joinAlias" autocomplete="off" :placeholder="t('settings.sync.join.alias')" />
        <Input
          v-model="joinPassword"
          type="password"
          autocomplete="off"
          :placeholder="t('settings.sync.join.password')"
          @keydown.enter="join"
        />
        <p v-if="joinError" class="text-[11px] text-destructive">{{ joinError }}</p>
        <p v-if="relayReady === false" class="text-[11px] text-amber-600 dark:text-amber-400">
          {{ t('settings.sync.relayMissing') }}
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <Button
            size="sm"
            :disabled="joining || relayReady === false || !joinAlias.trim() || !joinPassword || !!joinError"
            @click="join"
          >
            <component :is="joining ? Loader2 : LogIn" class="h-3.5 w-3.5" :class="{ 'animate-spin': joining }" :stroke-width="1.75" />
            {{ t('settings.sync.join.button') }}
          </Button>
          <span v-if="joining" class="text-[11px] text-muted-foreground">{{ t('settings.sync.join.waiting') }}</span>
        </div>
      </section>
    </template>

    <!-- In a group -->
    <template v-else>
      <div class="flex border-b border-border/60 text-xs font-medium" role="tablist">
        <button
          v-for="id in TABS"
          :key="id"
          type="button"
          role="tab"
          :aria-selected="tab === id"
          class="px-3 py-2 transition-colors cursor-pointer"
          :class="tab === id ? 'text-foreground border-b-2 border-primary' : 'text-muted-foreground hover:text-foreground'"
          @click="tab = id"
        >
          {{ t(`settings.sync.tabs.${id}`) }}
        </button>
      </div>

      <!-- Overview -->
      <template v-if="tab === 'overview'">
        <section class="space-y-3">
          <p class="text-xs font-medium">{{ t('settings.sync.ondemand.header') }}</p>
          <p v-if="!otherDevices.length" class="text-[11px] text-muted-foreground">{{ t('settings.sync.ondemand.noSource') }}</p>
          <template v-else>
            <div class="flex items-center gap-2">
              <span class="shrink-0 text-xs text-muted-foreground">{{ t('settings.sync.ondemand.source') }}</span>
              <AppSelect
                class="w-56"
                :model-value="source ?? ''"
                :options="sourceOptions"
                :disabled="store.busy"
                @update:model-value="changeSource"
              />
              <span class="flex items-center gap-1 text-[11px] text-muted-foreground">
                <span class="h-2 w-2 rounded-full" :class="sourceOnline ? 'bg-emerald-500' : 'bg-muted-foreground/40'" />
                {{ sourceOnline ? t('settings.sync.ondemand.online') : t('settings.sync.ondemand.offline') }}
              </span>
            </div>
            <div class="space-y-2.5 rounded-lg border border-border/60 bg-muted/20 p-3">
              <Button size="sm" :disabled="!source || !sourceOnline" @click="store.busy ? (store.dialogOpen = true) : syncFromSource()">
                <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.busy }" :stroke-width="1.75" />
                {{ t('settings.sync.ondemand.syncFrom', { name: sourceName }) }}
              </Button>
              <p v-if="source && !sourceOnline" class="text-[11px] text-amber-600 dark:text-amber-400">
                {{ t('settings.sync.ondemand.offlineHelp', { name: sourceName }) }}
              </p>
              <label class="flex cursor-pointer items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  class="accent-primary"
                  :checked="store.includeFolders"
                  @change="store.setIncludeFolders(($event.target as HTMLInputElement).checked)"
                >
                {{ t('settings.sync.ondemand.includeFolders', { n: folderCount }) }}
              </label>
            </div>
          </template>
          <div class="space-y-1 text-[11px] text-muted-foreground">
            <p v-if="!lastRun">{{ t('settings.sync.ondemand.lastNever') }}</p>
            <template v-else>
              <p>
                {{ t('settings.sync.ondemand.last', {
                  ago: ago(lastRun.at),
                  name: store.deviceName(lastRun.source_device),
                  result: lastRun.ok ? t('settings.sync.ondemand.resultOk') : t('settings.sync.ondemand.resultFailed'),
                }) }}
                <template v-if="lastRun.ok || lastRun.projects_total">
                  — {{ t('settings.sync.ondemand.lastData', { n: lastRun.applied }) }}
                  <template v-if="lastRun.projects_total">
                    · {{ t('settings.sync.ondemand.lastProjects', { ok: lastRun.projects_ok, total: lastRun.projects_total }) }}
                  </template>
                </template>
              </p>
              <p v-if="lastRun.error" class="break-words text-destructive">{{ errorText(lastRun.error) }}</p>
              <details v-if="lastRun.project_errors.length">
                <summary class="flex cursor-pointer list-none items-center gap-0.5 text-destructive">
                  <ChevronRight class="h-3 w-3" :stroke-width="1.75" />
                  {{ t('settings.sync.ondemand.lastProjectErrors', { n: lastRun.project_errors.length }) }}
                </summary>
                <ul class="mt-1 space-y-0.5 pl-4">
                  <li v-for="e in lastRun.project_errors" :key="e.project_id" class="break-words">
                    <span class="text-foreground">{{ e.name }}</span>: {{ errorText(e.message) }}
                  </li>
                </ul>
              </details>
            </template>
            <p>{{ t('settings.sync.ondemand.pending', { n: status.pending }) }}</p>
            <p v-if="status.paused" class="text-amber-600 dark:text-amber-400">{{ t('settings.sync.ondemand.pauseHelp') }}</p>
          </div>
          <p v-if="status.last_error && status.state !== 'online'" class="break-words text-[11px] text-destructive">
            {{ errorText(status.last_error) }}
          </p>
          <Button v-if="relayBehind" variant="outline" size="sm" :disabled="rebootstrapping" @click="rebootstrap">
            <RotateCcw class="h-3.5 w-3.5" :class="{ 'animate-spin': rebootstrapping }" :stroke-width="1.75" />
            {{ rebootstrapping ? t('settings.sync.rebootstrap.busy') : t('settings.sync.rebootstrap.button') }}
          </Button>
        </section>

        <!-- Changes the relay refused for good -->
        <section v-if="status.parked > 0 || parked.length" class="space-y-2 rounded-lg border border-orange-500/40 bg-orange-500/5 p-3">
          <label class="flex items-center gap-1.5 text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
            <AlertTriangle class="h-3 w-3 text-orange-500" :stroke-width="1.75" />
            {{ t('settings.sync.parked.title') }}
            <Badge variant="secondary">{{ Math.max(status.parked, parked.length) }}</Badge>
          </label>
          <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.parked.help') }}</p>
          <ul v-if="parked.length" class="max-h-48 divide-y divide-border/60 overflow-y-auto rounded-md border border-border/60">
            <li v-for="r in parked" :key="`${r.tbl}:${r.pk}`" class="flex items-center gap-2 px-2.5 py-1.5 text-xs">
              <span class="shrink-0 font-mono text-muted-foreground">{{ r.tbl }}</span>
              <span class="min-w-0 flex-1 truncate font-mono" :title="r.pk">{{ r.pk }}</span>
              <Badge variant="secondary">{{ parkedReason(r.reason) }}</Badge>
              <span class="shrink-0 text-[11px] text-muted-foreground">{{ t('settings.sync.parked.when', { ago: ago(r.parked_at) }) }}</span>
            </li>
          </ul>
          <p v-if="status.parked > parked.length && parked.length" class="text-[11px] text-muted-foreground">
            {{ t('settings.sync.parked.more', { n: parked.length }) }}
          </p>
          <Button variant="outline" size="sm" :disabled="retrying" @click="retryParked">
            <component :is="retrying ? Loader2 : RotateCcw" class="h-3.5 w-3.5" :class="{ 'animate-spin': retrying }" :stroke-width="1.75" />
            {{ t('settings.sync.parked.retry') }}
          </Button>
        </section>

        <div class="border-t border-border/60" />

        <section class="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            :disabled="resyncing"
            :title="t('settings.sync.resync.hint')"
            @click="resyncAll"
          >
            <component :is="resyncing ? Loader2 : RotateCcw" class="h-3.5 w-3.5" :class="{ 'animate-spin': resyncing }" :stroke-width="1.75" />
            {{ t('settings.sync.resync.button') }}
          </Button>
          <Button variant="outline" size="sm" :disabled="busyPause" @click="togglePause">
            <component :is="status.paused ? Play : Pause" class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ status.paused ? t('settings.sync.status.resume') : t('settings.sync.status.pause') }}
          </Button>
          <Button variant="destructive" size="sm" class="ml-auto" :disabled="leaving" @click="leave">
            <component :is="leaving ? Loader2 : LogOut" class="h-3.5 w-3.5" :class="{ 'animate-spin': leaving }" :stroke-width="1.75" />
            {{ t('settings.sync.leave.button') }}
          </Button>
        </section>
      </template>

      <SyncDataTab v-else-if="tab === 'data'" />
      <SyncProjectsTab v-else-if="tab === 'projects'" />

      <!-- Devices -->
      <template v-else>
        <!-- Devices + leader -->
        <section class="space-y-3">
          <label class="flex items-center gap-1.5 text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
            <Users class="h-3 w-3" :stroke-width="1.75" />
            {{ t('settings.sync.devices.title') }}
          </label>
          <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.devices.leaderHelp') }}</p>
          <p class="text-[11px] text-muted-foreground">
            {{ canRemove ? t('settings.sync.devices.removeHelp')
              : t('settings.sync.devices.onlyCreator', { name: creator ? deviceLabel(creator) : '' }) }}
          </p>
          <ul class="divide-y divide-border/60 rounded-lg border border-border/60">
            <li v-for="d in devices" :key="d.id" class="flex items-center gap-2 px-3 py-2">
              <input
                type="radio"
                name="sync-leader"
                class="accent-primary"
                :checked="d.is_leader"
                :disabled="busyDevice !== null"
                :aria-label="t('settings.sync.devices.leader')"
                @change="setLeader(d.id)"
              >
              <div class="min-w-0 flex-1">
                <div v-if="d.is_self && renaming" class="flex items-center gap-1.5">
                  <Input v-model="newName" class="h-7" maxlength="64" @keydown.enter="saveRename" @keydown.esc="renaming = false" />
                  <Button size="xs" :disabled="!newName.trim() || busyDevice !== null" @click="saveRename">
                    {{ t('settings.sync.devices.save') }}
                  </Button>
                  <Button variant="ghost" size="xs" @click="renaming = false">{{ t('settings.sync.devices.cancel') }}</Button>
                </div>
                <template v-else>
                  <p class="flex items-center gap-1.5 truncate text-sm">
                    {{ deviceLabel(d) }}
                    <Badge v-if="d.is_self" variant="secondary">{{ t('settings.sync.devices.thisDevice') }}</Badge>
                    <Crown v-if="d.is_leader" class="h-3 w-3 text-amber-500" :stroke-width="2" :title="t('settings.sync.devices.leader')" />
                    <Badge v-if="d.is_creator" variant="secondary">{{ t('settings.sync.devices.creator') }}</Badge>
                  </p>
                  <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.devices.lastSeen', { ago: ago(d.last_seen) }) }}</p>
                </template>
              </div>
              <Button
                v-if="d.is_self && !renaming"
                variant="ghost"
                size="xs"
                :title="t('settings.sync.devices.rename')"
                @click="startRename"
              >
                <Pencil class="h-3 w-3" :stroke-width="1.75" />
              </Button>
              <Button
                v-if="!d.is_self && canRemove"
                variant="ghost"
                size="xs"
                :disabled="busyDevice !== null"
                :title="t('settings.sync.devices.remove')"
                @click="removeDevice(d)"
              >
                <component :is="busyDevice === d.id ? Loader2 : Trash2" class="h-3 w-3" :class="{ 'animate-spin': busyDevice === d.id }" :stroke-width="1.75" />
              </Button>
            </li>
            <li class="flex items-center gap-2 px-3 py-2">
              <input
                type="radio"
                name="sync-leader"
                class="accent-primary"
                :checked="leaderId === null"
                :disabled="busyDevice !== null"
                @change="setLeader(null)"
              >
              <span class="text-xs text-muted-foreground">{{ t('settings.sync.devices.noLeader') }}</span>
            </li>
          </ul>
        </section>

        <div class="border-t border-border/60" />

        <!-- Accept a new device -->
        <section class="space-y-3">
          <label class="flex items-center gap-1.5 text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
            <UserPlus class="h-3 w-3" :stroke-width="1.75" />
            {{ t('settings.sync.accept.title') }}
          </label>
          <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.accept.help') }}</p>
          <div v-if="acceptUntil === null">
            <Button variant="outline" size="sm" :disabled="acceptBusy" @click="openAccept">
              <component :is="acceptBusy ? Loader2 : UserPlus" class="h-3.5 w-3.5" :class="{ 'animate-spin': acceptBusy }" :stroke-width="1.75" />
              {{ t('settings.sync.accept.button') }}
            </Button>
          </div>
          <template v-else>
            <div class="flex flex-wrap items-center gap-2 rounded-lg border border-sky-500/40 bg-sky-500/5 px-3 py-2">
              <Loader2 class="h-3.5 w-3.5 animate-spin text-sky-500" :stroke-width="1.75" />
              <span class="text-xs">{{ t('settings.sync.accept.open', { time: countdown(acceptUntil, tick) }) }}</span>
              <Button variant="ghost" size="xs" class="ml-auto" @click="closeAccept">{{ t('settings.sync.accept.cancel') }}</Button>
            </div>
            <div
              v-if="joinRequest"
              role="alertdialog"
              class="space-y-2 rounded-lg border border-amber-500/50 bg-amber-500/5 p-3"
            >
              <p class="text-sm font-medium">{{ t('settings.sync.accept.request.title', { name: joinRequest.device_name }) }}</p>
              <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.accept.request.message') }}</p>
              <div class="flex flex-wrap items-center gap-2">
                <Button size="sm" :disabled="answering" @click="answer(true)">{{ t('settings.sync.accept.request.allow') }}</Button>
                <Button variant="outline" size="sm" :disabled="answering" @click="answer(false)">
                  {{ t('settings.sync.accept.request.deny') }}
                </Button>
                <span class="ml-auto text-[11px] tabular-nums text-muted-foreground">
                  {{ t('settings.sync.accept.request.expires', { time: countdown(joinRequest.expires_at, tick) }) }}
                </span>
              </div>
            </div>
          </template>
        </section>
      </template>
    </template>

    <!-- Result of the first-join merge -->
    <section v-if="commitReport && paired" class="space-y-2 rounded-lg border border-border/60 bg-muted/20 p-3">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
        {{ t('settings.sync.join.reportTitle') }}
      </label>
      <ul class="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        <li v-for="[key, n] in totalRows(commitReport.preview)" :key="key" class="flex justify-between gap-2">
          <span class="truncate text-muted-foreground">{{ t(`settings.sync.join.counts.${key}`) }}</span>
          <span class="tabular-nums">{{ n }}</span>
        </li>
        <li class="flex justify-between gap-2">
          <span class="truncate text-muted-foreground">{{ t('settings.sync.join.counts.queued') }}</span>
          <span class="tabular-nums">{{ commitReport.queued }}</span>
        </li>
      </ul>
    </section>
  </Card>
</template>
