<script setup lang="ts">
// Device pull sync: pick another online Devdy and type its master password, tick what to
// take, then pull once and overwrite local data (see docs/device-pull-sync-plan.md).
// Steps: device (+ password) → options → (confirm, when mirror deletes) → running → result.
// A wrong password goes back to the device step with the password kept and focused.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  AlertTriangle, ArrowDownToLine, Check, ChevronRight, Circle, FolderX, Info, Link2Off, Loader2,
  Monitor, RefreshCw, RotateCcw, Trash2, WifiOff,
} from 'lucide-vue-next'
import { Banner, Button, Checkbox, Input, Modal } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import {
  DEVICE_SYNC_PROGRESS, REPORT_COLUMNS, SYNC_CATEGORY_KEYS, SYNC_GROUPS, SYNC_PHASES, WARNING_PREVIEW,
  compactReport, defaultCategories, deviceLabel, deviceLabels, groupState, groupWarnings, isDeviceGone,
  mirrorDeletions, needsRelogin, progressPercent, reportTotals, selectedCategories, syncErrorCode,
  type SyncCategories, type SyncCategory, type SyncDevice, type SyncProgress, type SyncReport, type SyncWarning,
  type SyncWarningGroup,
} from '@/lib/deviceSync'
import { useDeviceSyncRefresh } from '@/composables/useDeviceSyncRefresh'
import { useRemoteControlStore } from '@/stores/remoteControl'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const { t, te } = useI18n()
const { refreshAfterSync } = useDeviceSyncRefresh()
const store = useRemoteControlStore()

type Step = 'device' | 'options' | 'confirm' | 'running' | 'result'
const step = ref<Step>('device')

/** How often the device list is looked up again while it is empty. */
const EMPTY_POLL_MS = 5000

// ---- Step 1: online devices + master password ----
const devices = ref<SyncDevice[]>([])
const loadingDevices = ref(false)
const devicesLoaded = ref(false)
const devicesError = ref('')
const selectedId = ref<string | null>(null)
const selectedDevice = computed(() => devices.value.find(d => d.deviceId === selectedId.value) ?? null)
const labels = computed(() => deviceLabels(devices.value))
const selectedName = computed(() => (selectedDevice.value ? labels.value[selectedDevice.value.deviceId] ?? deviceLabel(selectedDevice.value) : ''))
const relayHost = computed(() => {
  try {
    return new URL(store.status?.relay_url ?? '').host
  } catch {
    return ''
  }
})
const password = ref('')
/** The last sync failed on the master password: the field is kept and highlighted. */
const passwordError = ref(false)
/** Why the last sync failed, shown on top of the step it went back to. */
const syncError = ref('')
const passwordEl = ref<HTMLElement | null>(null)
const canNext = computed(() => !!selectedDevice.value && !!password.value)

async function focusPassword(select = false) {
  await nextTick()
  const input = passwordEl.value?.querySelector('input')
  input?.focus({ preventScroll: true })
  if (select) input?.select()
}

async function loadDevices() {
  if (loadingDevices.value) return
  loadingDevices.value = true
  devicesError.value = ''
  try {
    devices.value = await invoke<SyncDevice[]>('device_sync_list_devices')
    if (selectedId.value && !devices.value.some(d => d.deviceId === selectedId.value)) selectedId.value = null
    // Only one device: nothing to choose.
    if (!selectedId.value && devices.value.length === 1) {
      selectedId.value = devices.value[0].deviceId
      if (step.value === 'device') void focusPassword()
    }
  } catch (e) {
    devices.value = []
    devicesError.value = errorMessage(e)
  } finally {
    loadingDevices.value = false
    devicesLoaded.value = true
  }
}

function pick(d: SyncDevice) {
  if (selectedId.value !== d.deviceId) {
    selectedId.value = d.deviceId
    passwordError.value = false
    syncError.value = ''
  }
  void focusPassword()
}

function next() {
  if (canNext.value) step.value = 'options'
}

// Look again every few seconds while nobody is online (only while this dialog is open).
let pollTimer: ReturnType<typeof setInterval> | null = null
function stopPolling() {
  if (pollTimer) clearInterval(pollTimer)
  pollTimer = null
}
watch(
  () => props.open && step.value === 'device' && devicesLoaded.value && !devices.value.length,
  (poll) => {
    stopPolling()
    if (poll) pollTimer = setInterval(() => void loadDevices(), EMPTY_POLL_MS)
  },
)

// ---- Step 2: categories ----
const categories = ref<SyncCategories>(defaultCategories())
const advancedOpen = ref(false)
const pickedCount = computed(() => selectedCategories(categories.value).length)
const allSelected = computed(() => pickedCount.value === SYNC_CATEGORY_KEYS.length)
/** Ticked parts whose local-only items mirror deletes: non-empty means a confirm step first. */
const mirrorParts = computed(() => mirrorDeletions(categories.value))

function toggleAll() {
  const v = !allSelected.value
  for (const k of SYNC_CATEGORY_KEYS) categories.value[k] = v
}

function setGroup(items: readonly SyncCategory[], v: boolean) {
  for (const k of items) categories.value[k] = v
}

function requestSync() {
  if (!canNext.value || !pickedCount.value) return
  if (mirrorParts.value.length) step.value = 'confirm'
  else void runSync()
}

// ---- Step 3/4: progress + report ----
const progress = ref<SyncProgress | null>(null)
const report = ref<SyncReport | null>(null)
const syncedSettings = ref(false)
const finishedAt = ref<Date | null>(null)
/** The device takes long to answer the connection: say so. */
const slowConnect = ref(false)
let slowTimer: ReturnType<typeof setTimeout> | null = null
let unlistenProgress: UnlistenFn | null = null

const percent = computed(() => progressPercent(progress.value))
const phaseIndex = computed(() => (progress.value ? SYNC_PHASES.indexOf(progress.value.phase) : 0))
const table = computed(() => (report.value ? compactReport(report.value) : { rows: [], columns: [], hidden: 0 }))
const totals = computed(() => (report.value ? reportTotals(report.value) : null))
const summaryParts = computed(() => {
  const s = totals.value
  if (!s) return []
  return REPORT_COLUMNS.filter(c => s[c] > 0).map(c => t(`deviceSync.result.total.${c}`, { n: s[c] }))
})
const relogin = computed(() => (report.value ? needsRelogin(report.value) : false))
const warningGroups = computed(() => (report.value ? groupWarnings(report.value.warnings) : []))
const warningCount = computed(() => warningGroups.value.reduce((n, g) => n + g.items.length, 0))
const expanded = ref(new Set<SyncWarningGroup>())

function toggleGroup(group: SyncWarningGroup) {
  const next = new Set(expanded.value)
  if (next.has(group)) next.delete(group)
  else next.add(group)
  expanded.value = next
}

function visibleWarnings(group: SyncWarningGroup, items: SyncWarning[]) {
  return expanded.value.has(group) ? items : items.slice(0, WARNING_PREVIEW)
}

const GROUP_ICON = { problems: AlertTriangle, folders: FolderX, links: Link2Off, mirror: Info } as const

/** Localised text of a report line; section / link kind params are translated too. */
function warningText(w: SyncWarning) {
  const params: Record<string, string> = { ...w.params }
  if (params.section) params.section = sectionLabel(params.section)
  if (params.kind) {
    const path = `deviceSync.linkKinds.${params.kind}`
    params.kind = te(path) ? t(path) : params.kind
  }
  const path = `deviceSync.warnings.${w.code}`
  return te(path) ? t(path, params) : (params.message ?? w.code)
}

/** Localised message for a known error code, else the backend text verbatim. */
function errorMessage(err: unknown, name = '') {
  const code = syncErrorCode(err)
  return code ? t(`deviceSync.errors.${code}`, { name }) : String(err)
}

function sectionLabel(key: string) {
  const path = `deviceSync.sections.${key}`
  return te(path) ? t(path) : key
}

function fmtMb(bytes: number) {
  return (bytes / 1_048_576).toLocaleString(undefined, { maximumFractionDigits: 1, minimumFractionDigits: 1 })
}

function clearSlow() {
  if (slowTimer) clearTimeout(slowTimer)
  slowTimer = null
  slowConnect.value = false
}

async function runSync() {
  const device = selectedDevice.value
  if (!device || !canNext.value) return
  const name = selectedName.value
  passwordError.value = false
  syncError.value = ''
  expanded.value = new Set()
  progress.value = { phase: 'connecting', received: 0, total: 0 }
  step.value = 'running'
  clearSlow()
  slowTimer = setTimeout(() => (slowConnect.value = progress.value?.phase === 'connecting'), 10_000)
  const picked = { ...categories.value }
  try {
    unlistenProgress = await listen<SyncProgress>(DEVICE_SYNC_PROGRESS, (e) => {
      if (e.payload) progress.value = e.payload
      if (e.payload?.phase !== 'connecting') slowConnect.value = false
    })
  } catch {
    // Outside Tauri: progress just stays on "connecting".
  }
  try {
    report.value = await invoke<SyncReport>('device_sync_pull', {
      deviceId: device.deviceId,
      categories: picked,
      password: password.value,
      sourceAlias: device.alias,
    })
    password.value = ''
    syncedSettings.value = picked.settings
    finishedAt.value = new Date()
    step.value = 'result'
    store.refreshLastSync().catch(() => {})
    await refreshAfterSync(picked)
  } catch (e) {
    const code = syncErrorCode(e)
    syncError.value = errorMessage(e, name)
    if (code === 'wrong_password' || isDeviceGone(code)) {
      // Back to the device step: fix the password, or pick a device that is still there.
      passwordError.value = code === 'wrong_password'
      step.value = 'device'
      if (passwordError.value) void focusPassword(true)
      else void loadDevices()
    } else {
      step.value = 'options'
    }
  } finally {
    clearSlow()
    unlistenProgress?.()
    unlistenProgress = null
  }
}

function reset() {
  step.value = 'device'
  selectedId.value = null
  devicesError.value = ''
  devicesLoaded.value = false
  categories.value = defaultCategories()
  advancedOpen.value = false
  password.value = ''
  passwordError.value = false
  syncError.value = ''
  expanded.value = new Set()
  progress.value = null
  report.value = null
  syncedSettings.value = false
  finishedAt.value = null
}

function close() {
  if (step.value === 'running') return
  emit('close')
}

// Settings (theme, language…) and screens that loaded before the sync: a reload picks it all up.
function reloadApp() {
  window.location.reload()
}

watch(
  () => props.open,
  (open) => {
    if (!open) {
      stopPolling()
      return
    }
    reset()
    void loadDevices()
  },
  { immediate: true },
)
onBeforeUnmount(() => {
  stopPolling()
  clearSlow()
  unlistenProgress?.()
})
</script>

<template>
  <Modal
    :open="open"
    :title="t('deviceSync.title')"
    size="md"
    scroll-body
    :closable="step !== 'running'"
    :dismiss-on-overlay="false"
    @close="close"
  >
    <!-- Step 1: pick a device + its master password -->
    <div v-if="step === 'device'" class="space-y-4 p-4">
      <div class="flex items-center gap-2">
        <p class="flex-1 text-[11px] text-muted-foreground">
          <span class="font-medium text-foreground">{{ t('deviceSync.stepOf', { n: 1, total: 2 }) }}</span>
          · {{ relayHost ? t('deviceSync.device.help', { host: relayHost }) : t('deviceSync.device.helpNoHost') }}
        </p>
        <Button
          variant="outline"
          size="icon-sm"
          :disabled="loadingDevices"
          :title="t('deviceSync.device.refresh')"
          :aria-label="t('deviceSync.device.refresh')"
          @click="loadDevices"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loadingDevices }" :stroke-width="1.75" />
        </Button>
      </div>

      <Banner v-if="devicesError" tone="destructive" live>{{ devicesError }}</Banner>

      <div v-if="!devicesLoaded" class="flex items-center justify-center gap-2 py-6 text-xs text-muted-foreground">
        <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ t('deviceSync.device.loading') }}
      </div>

      <!-- Nobody online: what the other machine needs -->
      <div v-else-if="!devices.length && !devicesError" class="flex flex-col items-center gap-3 py-4 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
          <WifiOff class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('deviceSync.device.empty') }}</p>
        <div class="w-full max-w-xs space-y-1.5 rounded-lg border border-border/60 bg-muted/20 p-3 text-left">
          <p class="text-[11px] font-medium text-muted-foreground">{{ t('deviceSync.device.emptyCheck') }}</p>
          <p v-for="k in ['relay', 'remote', 'password']" :key="k" class="flex items-center gap-2 text-xs">
            <Circle class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            {{ t(`deviceSync.device.needs.${k}`, { host: relayHost }) }}
          </p>
        </div>
        <p class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          <Loader2 class="h-3 w-3 animate-spin" :stroke-width="1.75" />{{ t('deviceSync.device.polling') }}
        </p>
      </div>

      <template v-else-if="devices.length">
        <ul class="divide-y divide-border/50 rounded-lg border border-border/60" role="listbox">
          <li v-for="d in devices" :key="d.deviceId">
            <button
              type="button"
              role="option"
              :aria-selected="selectedId === d.deviceId"
              class="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors cursor-pointer"
              :class="selectedId === d.deviceId ? 'bg-primary/15' : 'hover:bg-accent/50'"
              @click="pick(d)"
            >
              <Monitor class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <span class="min-w-0 flex-1">
                <span class="block truncate font-mono text-[13px] font-medium">{{ labels[d.deviceId] }}</span>
                <span class="block text-[11px] text-muted-foreground">
                  {{ d.appVersion ? t('deviceSync.device.version', { version: d.appVersion }) : t('deviceSync.device.versionUnknown') }}
                </span>
              </span>
              <Check v-if="selectedId === d.deviceId" class="h-3.5 w-3.5 shrink-0 text-primary" :stroke-width="2" />
            </button>
          </li>
        </ul>

        <Banner v-if="syncError" tone="destructive" live>
          <span class="block font-medium">{{ t('deviceSync.options.failed', { name: selectedName }) }}</span>
          <span class="block">{{ syncError }}</span>
        </Banner>

        <section v-if="selectedDevice" ref="passwordEl" class="space-y-1.5">
          <label class="text-xs font-medium text-muted-foreground">
            {{ t('deviceSync.options.password') }} <span class="font-mono text-foreground">{{ selectedName }}</span>
          </label>
          <Input
            v-model="password"
            type="password"
            autocomplete="off"
            :class="passwordError ? 'border-destructive' : ''"
            :aria-invalid="passwordError || undefined"
            @update:model-value="passwordError = false"
            @keydown.enter="next"
          />
          <p class="text-[11px] text-muted-foreground">{{ t('deviceSync.options.passwordHint', { name: selectedName }) }}</p>
        </section>
      </template>
    </div>

    <!-- Step 2: what to take -->
    <div v-else-if="step === 'options'" class="space-y-3 p-4">
      <Banner v-if="syncError" tone="destructive" live>
        <span class="block font-medium">{{ t('deviceSync.options.failed', { name: selectedName }) }}</span>
        <span class="block">{{ syncError }}</span>
      </Banner>
      <div class="flex items-center gap-2">
        <p class="flex-1 text-[11px] text-muted-foreground">
          <span class="font-medium text-foreground">{{ t('deviceSync.stepOf', { n: 2, total: 2 }) }}</span>
          · {{ t('deviceSync.options.from', { name: selectedName }) }}
        </p>
        <button type="button" class="text-[11px] text-primary hover:underline cursor-pointer" @click="toggleAll">
          {{ allSelected ? t('deviceSync.options.selectNone') : t('deviceSync.options.selectAll') }}
        </button>
      </div>

      <div v-for="g in SYNC_GROUPS" :key="g.key" class="rounded-lg border border-border/60">
        <div class="flex items-center gap-2 border-b border-border/50 bg-muted/20 px-3 py-2">
          <Checkbox
            class="flex-1"
            :model-value="groupState(categories, g.items)"
            @update:model-value="setGroup(g.items, $event)"
          >
            <span class="text-xs font-medium">{{ t(`deviceSync.groups.${g.key}`) }}</span>
          </Checkbox>
          <span class="text-[10px] tabular-nums text-muted-foreground">
            {{ g.items.filter(k => categories[k]).length }}/{{ g.items.length }}
          </span>
        </div>
        <div class="grid grid-cols-2 gap-x-3 gap-y-2 px-3 py-2.5">
          <Checkbox v-for="k in g.items" :key="k" v-model="categories[k]" :title="t(`deviceSync.rules.${k}`)">
            <span class="block text-xs">{{ t(`deviceSync.sections.${k}`) }}</span>
            <span class="block truncate text-[10px] text-muted-foreground">{{ t(`deviceSync.short.${k}`) }}</span>
          </Checkbox>
        </div>
      </div>

      <div class="space-y-2">
        <button
          type="button"
          class="flex items-center gap-1 text-[11px] font-medium text-muted-foreground hover:text-foreground cursor-pointer"
          :aria-expanded="advancedOpen"
          @click="advancedOpen = !advancedOpen"
        >
          <ChevronRight class="h-3 w-3 transition-transform" :class="advancedOpen && 'rotate-90'" :stroke-width="2" />
          {{ t('deviceSync.advanced.title') }}
          <span v-if="!advancedOpen" class="font-normal">
            · {{ categories.mirror ? t('deviceSync.advanced.mirrorOn') : t('deviceSync.advanced.mirrorOff') }}
          </span>
        </button>
        <div
          v-if="advancedOpen"
          class="rounded-lg border px-3 py-2"
          :class="categories.mirror ? 'border-destructive/40 bg-destructive/10' : 'border-border/60'"
        >
          <Checkbox v-model="categories.mirror" tone="destructive">
            <span class="block text-xs font-medium">{{ t('deviceSync.mirror.label') }}</span>
            <span class="block text-[11px] text-muted-foreground">{{ t('deviceSync.mirror.hint') }}</span>
          </Checkbox>
        </div>
        <p v-if="!categories.mirror" class="text-[11px] text-muted-foreground/80">{{ t('deviceSync.options.keepLocal') }}</p>
      </div>
    </div>

    <!-- Step 2b: confirm mirror deletions -->
    <div v-else-if="step === 'confirm'" class="space-y-3 p-4">
      <div class="flex gap-3">
        <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-destructive/10 text-destructive">
          <AlertTriangle class="h-4 w-4" :stroke-width="1.75" />
        </div>
        <div class="min-w-0 space-y-2">
          <p class="text-sm font-medium">{{ t('deviceSync.confirm.title', { name: selectedName }) }}</p>
          <p class="text-xs text-muted-foreground">{{ t('deviceSync.confirm.message') }}</p>
          <ul class="grid grid-cols-2 gap-1 text-xs">
            <li v-for="k in mirrorParts" :key="k" class="flex items-center gap-1.5 text-destructive">
              <Trash2 class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              {{ t(`deviceSync.sections.${k}`) }}
            </li>
          </ul>
          <p class="text-[11px] text-muted-foreground">{{ t('deviceSync.confirm.neverDeleted') }}</p>
        </div>
      </div>
    </div>

    <!-- Step 3: running -->
    <div v-else-if="step === 'running'" class="space-y-4 p-4">
      <p class="text-xs">{{ t('deviceSync.running.title', { n: pickedCount, name: selectedName }) }}</p>
      <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
        <div class="h-full bg-primary transition-all" :style="{ width: `${percent}%` }" />
      </div>
      <ol class="space-y-1.5 text-xs">
        <li
          v-for="(phase, i) in SYNC_PHASES.slice(0, -1)"
          :key="phase"
          class="flex items-center gap-2"
          :class="i > phaseIndex ? 'text-muted-foreground' : ''"
        >
          <Check v-if="i < phaseIndex" class="h-3.5 w-3.5 text-success" :stroke-width="2" />
          <Loader2 v-else-if="i === phaseIndex" class="h-3.5 w-3.5 animate-spin text-primary" :stroke-width="1.75" />
          <span v-else class="h-3.5 w-3.5" />
          {{ t(`deviceSync.phases.${phase}`, { name: selectedName }) }}
          <span
            v-if="phase === 'downloading' && i === phaseIndex && progress && progress.total > 0"
            class="tabular-nums text-[11px] text-muted-foreground"
          >{{ t('deviceSync.running.size', { percent, received: fmtMb(progress.received), total: fmtMb(progress.total) }) }}</span>
        </li>
      </ol>
      <p v-if="slowConnect" class="text-[11px] text-warning">{{ t('deviceSync.running.slow', { name: selectedName }) }}</p>
      <p class="text-[11px] text-muted-foreground">{{ t('deviceSync.running.keepOpen') }}</p>
    </div>

    <!-- Step 4: report -->
    <div v-else-if="report" class="space-y-3 p-4">
      <Banner tone="success">
        {{ t('deviceSync.result.done', { name: selectedName, time: finishedAt?.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) ?? '' }) }}
      </Banner>
      <p v-if="summaryParts.length" class="text-sm font-medium">{{ summaryParts.join(' · ') }}</p>

      <template v-if="table.rows.length">
        <table class="w-full text-xs">
          <thead class="text-[10px] uppercase tracking-wider text-muted-foreground">
            <tr>
              <th class="py-1 text-left font-medium" />
              <th v-for="c in table.columns" :key="c" class="text-right font-medium">{{ t(`deviceSync.result.${c}`) }}</th>
            </tr>
          </thead>
          <tbody class="tabular-nums">
            <tr v-for="row in table.rows" :key="row.key">
              <td class="py-0.5 text-muted-foreground">{{ sectionLabel(row.key) }}</td>
              <td
                v-for="c in table.columns"
                :key="c"
                class="text-right"
                :class="!row.counts[c] ? 'text-muted-foreground/50' : c === 'deleted' ? 'text-destructive' : c === 'skipped' ? 'text-muted-foreground' : ''"
              >{{ row.counts[c] || '–' }}</td>
            </tr>
          </tbody>
        </table>
        <p v-if="table.hidden" class="text-[11px] text-muted-foreground/70">{{ t('deviceSync.result.hidden', { n: table.hidden }) }}</p>
      </template>
      <p v-else class="text-[11px] text-muted-foreground">{{ t('deviceSync.result.nothing') }}</p>

      <div v-if="warningGroups.length" class="space-y-2 rounded-md border border-warning/30 bg-warning/10 p-2.5">
        <p class="flex items-center gap-1.5 text-[11px] font-medium">
          <AlertTriangle class="h-3.5 w-3.5 text-warning" :stroke-width="2" />{{ t('deviceSync.result.attention', { n: warningCount }) }}
        </p>
        <section v-for="g in warningGroups" :key="g.group" class="space-y-1">
          <p class="flex items-center gap-1.5 text-[11px] font-medium text-muted-foreground">
            <component :is="GROUP_ICON[g.group]" class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            {{ t(`deviceSync.warningGroups.${g.group}`, { n: g.items.length }) }}
          </p>
          <ul class="space-y-0.5 pl-[18px] text-[11px]">
            <li v-for="(w, i) in visibleWarnings(g.group, g.items)" :key="i" class="break-words">{{ warningText(w) }}</li>
          </ul>
          <button
            v-if="g.items.length > WARNING_PREVIEW"
            type="button"
            class="pl-[18px] text-[11px] text-primary hover:underline cursor-pointer"
            @click="toggleGroup(g.group)"
          >
            {{ expanded.has(g.group) ? t('deviceSync.result.showLess') : t('deviceSync.result.showMore', { n: g.items.length - WARNING_PREVIEW }) }}
          </button>
        </section>
      </div>
      <p v-if="relogin" class="text-[11px] text-muted-foreground">{{ t('deviceSync.result.reloginHint') }}</p>
      <p v-if="syncedSettings" class="text-[11px] text-muted-foreground">{{ t('deviceSync.result.reloadHint') }}</p>
    </div>

    <template #footer>
      <template v-if="step === 'device'">
        <Button variant="ghost" size="sm" @click="close">{{ t('deviceSync.actions.cancel') }}</Button>
        <Button size="sm" :disabled="!canNext" @click="next">{{ t('deviceSync.actions.next') }}</Button>
      </template>
      <template v-else-if="step === 'options'">
        <Button variant="ghost" size="sm" class="mr-auto" @click="step = 'device'">{{ t('deviceSync.actions.back') }}</Button>
        <Button variant="ghost" size="sm" @click="close">{{ t('deviceSync.actions.cancel') }}</Button>
        <Button v-if="mirrorParts.length" variant="destructive" size="sm" :disabled="!pickedCount" @click="requestSync">
          {{ t('deviceSync.actions.confirmDelete') }}
        </Button>
        <Button v-else size="sm" :disabled="!pickedCount" @click="requestSync">
          <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('deviceSync.actions.syncN', { n: pickedCount, name: selectedName }) }}
        </Button>
      </template>
      <template v-else-if="step === 'confirm'">
        <Button variant="ghost" size="sm" class="mr-auto" @click="step = 'options'">{{ t('deviceSync.actions.back') }}</Button>
        <Button variant="destructive" size="sm" @click="runSync">
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('deviceSync.actions.mirrorSync') }}
        </Button>
      </template>
      <template v-else-if="step === 'running'">
        <Button size="sm" disabled>
          <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
          {{ t('deviceSync.actions.syncing') }}
        </Button>
      </template>
      <template v-else-if="syncedSettings">
        <Button variant="ghost" size="sm" @click="close">{{ t('deviceSync.actions.close') }}</Button>
        <Button size="sm" @click="reloadApp">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('deviceSync.actions.reload') }}
        </Button>
      </template>
      <template v-else>
        <Button size="sm" @click="close">{{ t('deviceSync.actions.close') }}</Button>
      </template>
    </template>
  </Modal>
</template>
