<script setup lang="ts">
// Device pull sync: pick another online Devdy, tick what to take, enter its master
// password, then pull once and overwrite local data (see docs/device-pull-sync-plan.md).
// Steps: device → options → (confirm, when mirror deletes) → running → result.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle, Check, FolderX, Info, Link2Off, Loader2, Monitor, RefreshCw, RotateCcw, Trash2, WifiOff } from 'lucide-vue-next'
import { Banner, Button, Input, Modal } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import {
  DEVICE_SYNC_PROGRESS, SYNC_CATEGORY_KEYS, SYNC_PHASES, WARNING_PREVIEW, defaultCategories,
  deviceLabel, groupWarnings, isDeviceGone, mirrorDeletions, progressPercent, reportRows,
  selectedCategories, syncErrorCode,
  type SyncCategories, type SyncDevice, type SyncProgress, type SyncReport, type SyncWarning,
  type SyncWarningGroup,
} from '@/lib/deviceSync'
import { useDeviceSyncRefresh } from '@/composables/useDeviceSyncRefresh'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const { t, te } = useI18n()
const { refreshAfterSync } = useDeviceSyncRefresh()

type Step = 'device' | 'options' | 'confirm' | 'running' | 'result'
const step = ref<Step>('device')

// ---- Step 1: online devices ----
const devices = ref<SyncDevice[]>([])
const loadingDevices = ref(false)
const devicesError = ref('')
const selectedId = ref<string | null>(null)
const selectedDevice = computed(() => devices.value.find(d => d.deviceId === selectedId.value) ?? null)
const selectedName = computed(() => (selectedDevice.value ? deviceLabel(selectedDevice.value) : ''))

async function loadDevices() {
  if (loadingDevices.value) return
  loadingDevices.value = true
  devicesError.value = ''
  try {
    devices.value = await invoke<SyncDevice[]>('device_sync_list_devices')
    if (selectedId.value && !devices.value.some(d => d.deviceId === selectedId.value)) selectedId.value = null
  } catch (e) {
    devices.value = []
    devicesError.value = errorMessage(e)
  } finally {
    loadingDevices.value = false
  }
}

// ---- Step 2: categories + password ----
const categories = ref<SyncCategories>(defaultCategories())
const password = ref('')
/** The last sync failed on the master password: the field is kept and highlighted. */
const passwordError = ref(false)
/** Why the last sync failed, shown at the top of step 2. */
const syncError = ref('')
const optionsEl = ref<HTMLElement | null>(null)
const passwordEl = ref<HTMLElement | null>(null)
const allSelected = computed(() => SYNC_CATEGORY_KEYS.every(k => categories.value[k]))
const canSync = computed(() => !!selectedDevice.value && selectedCategories(categories.value).length > 0 && !!password.value)
/** Ticked parts whose local-only items mirror deletes: non-empty means a confirm step first. */
const mirrorParts = computed(() => mirrorDeletions(categories.value))
const mirrorPartsLabel = computed(() => mirrorParts.value.map(k => t(`deviceSync.sections.${k}`)).join(', '))

function toggleAll() {
  const next = !allSelected.value
  for (const k of SYNC_CATEGORY_KEYS) categories.value[k] = next
}

function requestSync() {
  if (!canSync.value) return
  if (mirrorParts.value.length) step.value = 'confirm'
  else void runSync()
}

// ---- Step 3/4: progress + report ----
const progress = ref<SyncProgress | null>(null)
const report = ref<SyncReport | null>(null)
const syncedSettings = ref(false)
let unlistenProgress: UnlistenFn | null = null

const percent = computed(() => progressPercent(progress.value))
const phaseIndex = computed(() => (progress.value ? SYNC_PHASES.indexOf(progress.value.phase) : 0))
const rows = computed(() => (report.value ? reportRows(report.value) : []))
const warningGroups = computed(() => (report.value ? groupWarnings(report.value.warnings) : []))
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

/** Back to step 2 with the error on top; a wrong password keeps the field and focuses it. */
async function showSyncError(message: string, wrongPassword: boolean) {
  syncError.value = message
  passwordError.value = wrongPassword
  step.value = 'options'
  await nextTick()
  if (wrongPassword) {
    const input = passwordEl.value?.querySelector('input')
    input?.focus({ preventScroll: true })
    input?.select()
  }
  optionsEl.value?.scrollIntoView({ block: 'start' })
}

async function runSync() {
  const device = selectedDevice.value
  if (!device || !canSync.value) return
  passwordError.value = false
  syncError.value = ''
  expanded.value = new Set()
  progress.value = { phase: 'connecting', received: 0, total: 0 }
  step.value = 'running'
  const picked = { ...categories.value }
  try {
    unlistenProgress = await listen<SyncProgress>(DEVICE_SYNC_PROGRESS, (e) => {
      if (e.payload) progress.value = e.payload
    })
  } catch {
    // Outside Tauri: progress just stays on "connecting".
  }
  try {
    report.value = await invoke<SyncReport>('device_sync_pull', {
      deviceId: device.deviceId,
      categories: picked,
      password: password.value,
    })
    password.value = ''
    syncedSettings.value = picked.settings
    step.value = 'result'
    await refreshAfterSync(picked)
  } catch (e) {
    const code = syncErrorCode(e)
    await showSyncError(errorMessage(e, deviceLabel(device)), code === 'wrong_password')
    // The device may be gone: refresh the list for when the user goes back to it.
    if (isDeviceGone(code)) void loadDevices()
  } finally {
    unlistenProgress?.()
    unlistenProgress = null
  }
}

function reset() {
  step.value = 'device'
  selectedId.value = null
  devicesError.value = ''
  categories.value = defaultCategories()
  password.value = ''
  passwordError.value = false
  syncError.value = ''
  expanded.value = new Set()
  progress.value = null
  report.value = null
  syncedSettings.value = false
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
    if (!open) return
    reset()
    void loadDevices()
  },
  { immediate: true },
)
onBeforeUnmount(() => unlistenProgress?.())
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
    <!-- Step 1: pick a device -->
    <div v-if="step === 'device'" class="space-y-3 p-4">
      <div class="flex items-center gap-2">
        <p class="flex-1 text-[11px] text-muted-foreground">{{ t('deviceSync.device.help') }}</p>
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

      <div v-if="loadingDevices && !devices.length" class="flex items-center gap-2 py-6 justify-center text-xs text-muted-foreground">
        <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ t('deviceSync.device.loading') }}
      </div>

      <div v-else-if="!devices.length" class="flex flex-col items-center gap-2 py-6 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
          <WifiOff class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('deviceSync.device.empty') }}</p>
        <p class="max-w-sm text-[11px] text-muted-foreground">{{ t('deviceSync.device.emptyHint') }}</p>
      </div>

      <ul v-else class="divide-y divide-border/50 rounded-lg border border-border/60" role="listbox">
        <li v-for="d in devices" :key="d.deviceId">
          <button
            type="button"
            role="option"
            :aria-selected="selectedId === d.deviceId"
            class="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors cursor-pointer"
            :class="selectedId === d.deviceId ? 'bg-primary/15' : 'hover:bg-accent/50'"
            @click="selectedId = d.deviceId"
            @dblclick="selectedId = d.deviceId; step = 'options'"
          >
            <Monitor class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            <span class="min-w-0 flex-1">
              <span class="block truncate text-[13px] font-medium">{{ deviceLabel(d) }}</span>
              <span class="block text-[11px] text-muted-foreground">
                {{ d.appVersion ? t('deviceSync.device.version', { version: d.appVersion }) : t('deviceSync.device.versionUnknown') }}
              </span>
            </span>
            <Check v-if="selectedId === d.deviceId" class="h-3.5 w-3.5 shrink-0 text-primary" :stroke-width="2" />
          </button>
        </li>
      </ul>
    </div>

    <!-- Step 2: categories + master password -->
    <div v-else-if="step === 'options'" ref="optionsEl" class="space-y-4 p-4">
      <Banner v-if="syncError" tone="destructive" live>
        <span class="block font-medium">{{ t('deviceSync.options.failed', { name: selectedName }) }}</span>
        <span class="block">{{ syncError }}</span>
      </Banner>
      <p class="text-[11px] text-muted-foreground">
        {{ t('deviceSync.options.from', { name: selectedName }) }}
      </p>

      <section class="space-y-2">
        <div class="flex items-center justify-between">
          <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            {{ t('deviceSync.options.what') }}
          </label>
          <button type="button" class="text-[11px] text-primary hover:underline cursor-pointer" @click="toggleAll">
            {{ allSelected ? t('deviceSync.options.selectNone') : t('deviceSync.options.selectAll') }}
          </button>
        </div>
        <div class="divide-y divide-border/50 rounded-lg border border-border/60">
          <label
            v-for="key in SYNC_CATEGORY_KEYS"
            :key="key"
            class="flex items-start gap-2.5 px-3 py-2 text-xs cursor-pointer hover:bg-accent/50"
          >
            <input v-model="categories[key]" type="checkbox" class="mt-0.5 accent-primary">
            <span class="min-w-0">
              <span class="block font-medium">{{ t(`deviceSync.sections.${key}`) }}</span>
              <span class="block text-[11px] text-muted-foreground">{{ t(`deviceSync.rules.${key}`) }}</span>
            </span>
          </label>
        </div>
      </section>

      <section ref="passwordEl" class="space-y-1.5">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
          {{ t('deviceSync.options.password') }}
        </label>
        <Input
          v-model="password"
          type="password"
          autocomplete="off"
          :class="passwordError ? 'border-destructive focus-visible:ring-destructive/40' : ''"
          :aria-invalid="passwordError || undefined"
          :placeholder="t('deviceSync.options.passwordPlaceholder', { name: selectedName })"
          @update:model-value="passwordError = false"
          @keydown.enter="requestSync"
        />
        <p v-if="passwordError" class="text-[11px] text-destructive">{{ t('deviceSync.options.passwordWrong') }}</p>
        <p v-else class="text-[11px] text-muted-foreground">{{ t('deviceSync.options.passwordHint') }}</p>
      </section>

      <!-- Mirror: separate from the list, off by default -->
      <label
        class="flex items-start gap-2.5 rounded-lg border px-3 py-2 text-xs cursor-pointer"
        :class="categories.mirror ? 'border-destructive/40 bg-destructive/10' : 'border-border/60 hover:bg-accent/50'"
      >
        <input v-model="categories.mirror" type="checkbox" class="mt-0.5 accent-destructive">
        <span class="min-w-0">
          <span class="block font-medium">{{ t('deviceSync.mirror.label') }}</span>
          <span class="block text-[11px] text-muted-foreground">{{ t('deviceSync.mirror.hint') }}</span>
        </span>
      </label>

      <Banner v-if="mirrorParts.length" tone="destructive" :icon="Trash2">
        {{ t('deviceSync.mirror.warning', { parts: mirrorPartsLabel }) }}
      </Banner>
      <Banner v-else tone="warning">{{ t('deviceSync.options.overwriteWarning') }}</Banner>
    </div>

    <!-- Step 2b: confirm mirror deletions -->
    <div v-else-if="step === 'confirm'" class="space-y-3 p-4">
      <div class="flex gap-3">
        <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-destructive/10 text-destructive">
          <AlertTriangle class="h-4 w-4" :stroke-width="1.75" />
        </div>
        <div class="min-w-0 space-y-2">
          <p class="text-sm font-medium">{{ t('deviceSync.confirm.title') }}</p>
          <p class="text-xs text-muted-foreground">
            {{ t('deviceSync.confirm.message', { name: selectedName }) }}
          </p>
          <ul class="space-y-1 text-xs">
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
      <p class="text-xs">{{ t('deviceSync.running.title', { name: selectedName }) }}</p>
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
          {{ t(`deviceSync.phases.${phase}`) }}
          <span
            v-if="phase === 'downloading' && i === phaseIndex && progress && progress.total > 0"
            class="tabular-nums text-[11px] text-muted-foreground"
          >{{ percent }}%</span>
        </li>
      </ol>
      <p class="text-[11px] text-muted-foreground">{{ t('deviceSync.running.keepOpen') }}</p>
    </div>

    <!-- Step 4: report -->
    <div v-else-if="report" class="space-y-3 p-4">
      <Banner tone="success">{{ t('deviceSync.result.done', { name: selectedName }) }}</Banner>
      <table v-if="rows.length" class="w-full text-xs">
        <thead class="text-[10px] uppercase tracking-wider text-muted-foreground">
          <tr>
            <th class="text-left font-medium" />
            <th class="text-right font-medium">{{ t('deviceSync.result.created') }}</th>
            <th class="text-right font-medium">{{ t('deviceSync.result.updated') }}</th>
            <th class="text-right font-medium">{{ t('deviceSync.result.unchanged') }}</th>
            <th class="text-right font-medium">{{ t('deviceSync.result.deleted') }}</th>
            <th class="text-right font-medium">{{ t('deviceSync.result.skipped') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in rows" :key="row.key">
            <td class="py-0.5 text-muted-foreground">{{ sectionLabel(row.key) }}</td>
            <td class="text-right tabular-nums">{{ row.counts.created }}</td>
            <td class="text-right tabular-nums">{{ row.counts.updated }}</td>
            <td class="text-right tabular-nums text-muted-foreground">{{ row.counts.unchanged }}</td>
            <td class="text-right tabular-nums" :class="row.counts.deleted ? 'text-destructive' : ''">{{ row.counts.deleted }}</td>
            <td class="text-right tabular-nums text-muted-foreground">{{ row.counts.skipped }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="text-[11px] text-muted-foreground">{{ t('deviceSync.result.nothing') }}</p>

      <section v-for="g in warningGroups" :key="g.group" class="space-y-1">
        <p
          class="flex items-center gap-1.5 text-[11px] font-medium"
          :class="g.group === 'problems' ? 'text-warning' : 'text-muted-foreground'"
        >
          <component :is="GROUP_ICON[g.group]" class="h-3 w-3 shrink-0" :stroke-width="1.75" />
          {{ t(`deviceSync.warningGroups.${g.group}`, { n: g.items.length }) }}
        </p>
        <ul class="space-y-0.5 pl-[18px] text-[11px]" :class="g.group === 'problems' ? 'text-warning' : 'text-muted-foreground'">
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
      <p v-if="categories.accounts" class="text-[11px] text-muted-foreground">{{ t('deviceSync.result.reloginHint') }}</p>
      <p v-if="syncedSettings" class="text-[11px] text-muted-foreground">{{ t('deviceSync.result.reloadHint') }}</p>
    </div>

    <template #footer>
      <template v-if="step === 'device'">
        <Button variant="ghost" size="sm" @click="close">{{ t('deviceSync.actions.cancel') }}</Button>
        <Button size="sm" :disabled="!selectedDevice" @click="step = 'options'">{{ t('deviceSync.actions.next') }}</Button>
      </template>
      <template v-else-if="step === 'options'">
        <Button variant="ghost" size="sm" class="mr-auto" @click="step = 'device'">{{ t('deviceSync.actions.back') }}</Button>
        <Button variant="ghost" size="sm" @click="close">{{ t('deviceSync.actions.cancel') }}</Button>
        <Button :variant="mirrorParts.length ? 'destructive' : 'primary'" size="sm" :disabled="!canSync" @click="requestSync">
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('deviceSync.actions.sync') }}
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
      <template v-else>
        <Button v-if="syncedSettings" variant="outline" size="sm" class="mr-auto" @click="reloadApp">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('deviceSync.actions.reload') }}
        </Button>
        <Button size="sm" @click="close">{{ t('deviceSync.actions.close') }}</Button>
      </template>
    </template>
  </Modal>
</template>
