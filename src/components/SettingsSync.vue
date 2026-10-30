<script setup lang="ts">
// Settings → Sync: two-way sync of the whole Devdy state between this user's machines over the
// relay (E2E encrypted). Not in a group → create one, or join another device's group with its
// device name + master password (that device must have "Accept a new device" open and approve the
// request; both need their own relay config). Joined but not merged (`sync_bootstrap_pending`, also after an
// app restart) → the merge step: preview (totals, per table, project matches) then commit, which
// starts the engine. In a group → status, sync now / pause, secrets toggle, devices (rename this
// one, remove others — creator only —, pick the leader that runs Pulse / Slack polling), project folders for this
// machine, re-sync, leave, and "Accept a new device" (5 min window + Allow / Deny per request,
// closed when this page goes away so a request is never left unseen).
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { open } from '@tauri-apps/plugin-dialog'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  Crown, FolderOpen, Loader2, LogIn, LogOut, Pause, Pencil, Play, Plus, RefreshCw, RotateCcw, Trash2, UserPlus, Users,
} from 'lucide-vue-next'
import { Badge, Button, Card, Input } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useSyncStore } from '@/stores/sync'
import {
  ageParts,
  canRemoveDevices,
  countdown,
  deviceLabel,
  groupCreator,
  joinErrorCode,
  onSyncAcceptWindow,
  onSyncJoinRequest,
  onSyncJoinResolved,
  syncAcceptClose,
  syncAcceptOpen,
  syncAcceptState,
  syncJoinRespond,
  syncRelayReady,
  missingProjects,
  sortDevices,
  syncBootstrapCommit,
  syncBootstrapPreview,
  syncCreateGroup,
  syncDeviceRename,
  syncDevicesList,
  syncGetSecrets,
  syncJoin,
  syncLeave,
  syncNow,
  syncPause,
  syncProjectMappingList,
  syncProjectMappingSet,
  syncResyncAll,
  syncRevokeDevice,
  syncSetLeader,
  syncSetSecrets,
  syncStateKey,
  syncTone,
  tableRows,
  totalRows,
  type BootstrapPreview,
  type BootstrapReport,
  type JoinRequest,
  type ProjectMapping,
  type SyncDevice,
} from '@/lib/sync'
import { DEVICE_ALIAS_RE } from '@/lib/remoteLogin'

const { t } = useI18n()
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

const DOT: Record<string, string> = {
  off: 'bg-muted-foreground/40',
  ok: 'bg-emerald-500',
  busy: 'bg-sky-500',
  warn: 'bg-amber-500',
  error: 'bg-destructive',
}

/** Backend errors with a known code get a translated message. */
function errorText(e: unknown): string {
  const code = joinErrorCode(e)
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
    previewError.value = String(e)
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
    toast.error(String(e))
  } finally {
    committing.value = false
  }
}

// ---- In a group ----
const busyNow = ref(false)
const busyPause = ref(false)
const secrets = ref(true)
const busySecrets = ref(false)

const devices = ref<SyncDevice[]>([])
const renaming = ref(false)
const newName = ref('')
const busyDevice = ref<string | null>(null)

const mappings = ref<ProjectMapping[]>([])
const busyMapping = ref<string | null>(null)

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
    toast.error(String(e))
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
    toast.error(String(e))
  } finally {
    answering.value = false
    if (joinRequest.value?.request_id === req.request_id) joinRequest.value = null
  }
}

async function loadDevices() {
  try {
    devices.value = sortDevices(await syncDevicesList())
  } catch (e) {
    devices.value = []
    toast.error(String(e))
  }
}

async function loadMappings() {
  try {
    mappings.value = await syncProjectMappingList()
  } catch (e) {
    mappings.value = []
    toast.error(String(e))
  }
}

async function loadSecrets() {
  secrets.value = await syncGetSecrets().catch(() => true)
}

async function loadPaired() {
  if (!paired.value) return
  await Promise.all([loadDevices(), loadMappings(), loadSecrets()])
}

async function runNow() {
  if (busyNow.value) return
  busyNow.value = true
  try {
    await syncNow()
  } catch (e) {
    toast.error(String(e))
  } finally {
    busyNow.value = false
  }
}

async function togglePause() {
  if (busyPause.value) return
  busyPause.value = true
  try {
    store.set(await syncPause(!status.value.paused))
  } catch (e) {
    toast.error(String(e))
  } finally {
    busyPause.value = false
  }
}

async function toggleSecrets() {
  if (busySecrets.value) return
  busySecrets.value = true
  const next = !secrets.value
  try {
    await syncSetSecrets(next)
    secrets.value = next
  } catch (e) {
    toast.error(String(e))
  } finally {
    busySecrets.value = false
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
    toast.error(String(e))
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
    toast.error(String(e).startsWith('only the device that created') ? t('settings.sync.devices.onlyCreatorError') : String(e))
  } finally {
    busyDevice.value = null
  }
}

async function setLeader(id: string | null) {
  if (id === leaderId.value) return
  busyDevice.value = id ?? 'none'
  try {
    await syncSetLeader(id)
    await loadDevices()
  } catch (e) {
    toast.error(String(e))
  } finally {
    busyDevice.value = null
  }
}

async function pickFolder(p: ProjectMapping) {
  const selected = await open({ directory: true, multiple: false, title: p.name })
  if (!selected || Array.isArray(selected)) return
  busyMapping.value = p.project_id
  try {
    await syncProjectMappingSet(p.project_id, selected)
    await loadMappings()
  } catch (e) {
    toast.error(String(e))
  } finally {
    busyMapping.value = null
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
    toast.error(String(e))
  } finally {
    resyncing.value = false
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
    const revoked = await syncLeave()
    await store.refresh()
    devices.value = []
    mappings.value = []
    preview.value = null
    previewError.value = ''
    commitReport.value = null
    if (revoked) toast.success(t('settings.sync.leave.done'))
    else toast.info(t('settings.sync.leave.offline'))
    relayReady.value = await syncRelayReady().catch(() => false)
  } catch (e) {
    toast.error(String(e))
  } finally {
    leaving.value = false
  }
}

onMounted(async () => {
  clock = setInterval(() => (now.value = Date.now()), 30_000)
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
        <span class="h-2 w-2 rounded-full" :class="DOT[tone]" />
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
      <section class="space-y-3">
        <div class="grid grid-cols-3 gap-2 text-xs">
          <div class="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
            <p class="text-[10px] uppercase tracking-wider text-muted-foreground">{{ t('settings.sync.status.lastPush') }}</p>
            <p class="mt-0.5">{{ ago(status.last_push) }}</p>
          </div>
          <div class="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
            <p class="text-[10px] uppercase tracking-wider text-muted-foreground">{{ t('settings.sync.status.lastPull') }}</p>
            <p class="mt-0.5">{{ ago(status.last_pull) }}</p>
          </div>
          <div class="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
            <p class="text-[10px] uppercase tracking-wider text-muted-foreground">{{ t('settings.sync.status.pending') }}</p>
            <p class="mt-0.5 tabular-nums">{{ status.pending }}</p>
          </div>
        </div>
        <p v-if="status.last_error && status.state !== 'online'" class="break-words text-[11px] text-destructive">
          {{ status.last_error }}
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <Button size="sm" :disabled="busyNow || status.paused" @click="runNow">
            <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': busyNow }" :stroke-width="1.75" />
            {{ t('settings.sync.status.now') }}
          </Button>
          <Button variant="outline" size="sm" :disabled="busyPause" @click="togglePause">
            <component :is="status.paused ? Play : Pause" class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ status.paused ? t('settings.sync.status.resume') : t('settings.sync.status.pause') }}
          </Button>
        </div>
      </section>

      <div class="flex items-center justify-between gap-4 rounded-lg border border-border/60 bg-muted/20 px-3.5 py-3">
        <div class="min-w-0">
          <p class="text-sm font-medium text-foreground">{{ t('settings.sync.secrets.label') }}</p>
          <p class="mt-0.5 text-[11px] text-muted-foreground">{{ t('settings.sync.secrets.hint') }}</p>
        </div>
        <button
          type="button"
          role="switch"
          :aria-checked="secrets"
          :aria-label="t('settings.sync.secrets.label')"
          :disabled="busySecrets"
          class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:opacity-50"
          :class="secrets ? 'bg-primary' : 'bg-muted'"
          @click="toggleSecrets"
        >
          <span
            class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform"
            :class="secrets ? 'translate-x-6' : 'translate-x-1'"
          />
        </button>
      </div>

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

      <div class="border-t border-border/60" />

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

      <!-- Project folders on this machine -->
      <section class="space-y-3">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.sync.projects.title') }}
        </label>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.projects.help') }}</p>
        <p v-if="!mappings.length" class="text-[11px] text-muted-foreground">{{ t('settings.sync.projects.empty') }}</p>
        <ul v-else class="divide-y divide-border/60 rounded-lg border border-border/60">
          <li v-for="p in mappings" :key="p.project_id" class="flex items-center gap-2 px-3 py-2">
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm">{{ p.name }}</p>
              <p
                class="truncate font-mono text-[11px]"
                :class="p.on_this_machine ? 'text-muted-foreground' : 'text-amber-600 dark:text-amber-400'"
                :title="p.on_this_machine ? p.path : p.synced_path"
              >
                {{ p.on_this_machine ? p.path : t('settings.sync.projects.notHere') }}
              </p>
            </div>
            <Button variant="outline" size="xs" :disabled="busyMapping !== null" @click="pickFolder(p)">
              <component :is="busyMapping === p.project_id ? Loader2 : FolderOpen" class="h-3 w-3" :class="{ 'animate-spin': busyMapping === p.project_id }" :stroke-width="1.75" />
              {{ p.on_this_machine ? t('settings.sync.projects.change') : t('settings.sync.projects.choose') }}
            </Button>
          </li>
        </ul>
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
        <Button variant="destructive" size="sm" class="ml-auto" :disabled="leaving" @click="leave">
          <component :is="leaving ? Loader2 : LogOut" class="h-3.5 w-3.5" :class="{ 'animate-spin': leaving }" :stroke-width="1.75" />
          {{ t('settings.sync.leave.button') }}
        </Button>
      </section>
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
