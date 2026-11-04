<script setup lang="ts">
// Device pull sync: pick another online Devdy, tick what to take, enter its master
// password, then pull once and overwrite local data (see docs/device-pull-sync-plan.md).
// Steps: device → options → (confirm, when mirror deletes) → running → result.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle, Check, Loader2, Monitor, RefreshCw, RotateCcw, Trash2, WifiOff } from 'lucide-vue-next'
import { Banner, Button, Input, Modal } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import {
  DEVICE_SYNC_PROGRESS, SYNC_CATEGORY_KEYS, SYNC_PHASES, defaultCategories, mirrorDeletions,
  progressPercent, reportRows, selectedCategories, syncErrorKind,
  type SyncCategories, type SyncDevice, type SyncProgress, type SyncReport,
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

async function loadDevices() {
  if (loadingDevices.value) return
  loadingDevices.value = true
  devicesError.value = ''
  try {
    devices.value = await invoke<SyncDevice[]>('device_sync_list_devices')
    if (selectedId.value && !devices.value.some(d => d.deviceId === selectedId.value)) selectedId.value = null
  } catch (e) {
    devices.value = []
    devicesError.value = String(e)
  } finally {
    loadingDevices.value = false
  }
}

// ---- Step 2: categories + password ----
const categories = ref<SyncCategories>(defaultCategories())
const password = ref('')
const passwordError = ref('')
const syncError = ref('')
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

function sectionLabel(key: string) {
  const path = `deviceSync.sections.${key}`
  return te(path) ? t(path) : key
}

async function runSync() {
  const device = selectedDevice.value
  if (!device || !canSync.value) return
  passwordError.value = ''
  syncError.value = ''
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
    const kind = syncErrorKind(e)
    if (kind === 'wrongPassword') {
      passwordError.value = t('deviceSync.errors.wrongPassword')
      step.value = 'options'
    } else if (kind === 'deviceOffline') {
      devicesError.value = t('deviceSync.errors.deviceOffline', { name: device.alias })
      selectedId.value = null
      step.value = 'device'
      void loadDevices()
    } else {
      syncError.value = String(e)
      step.value = 'options'
    }
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
  passwordError.value = ''
  syncError.value = ''
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
              <span class="block truncate text-[13px] font-medium">{{ d.alias }}</span>
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
    <div v-else-if="step === 'options'" class="space-y-4 p-4">
      <p class="text-[11px] text-muted-foreground">
        {{ t('deviceSync.options.from', { name: selectedDevice?.alias ?? '' }) }}
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

      <section class="space-y-1.5">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
          {{ t('deviceSync.options.password') }}
        </label>
        <Input
          v-model="password"
          type="password"
          autocomplete="off"
          :placeholder="t('deviceSync.options.passwordPlaceholder', { name: selectedDevice?.alias ?? '' })"
          @keydown.enter="requestSync"
        />
        <p v-if="passwordError" class="text-[11px] text-destructive">{{ passwordError }}</p>
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

      <Banner v-if="syncError" tone="destructive" live>{{ syncError }}</Banner>
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
            {{ t('deviceSync.confirm.message', { name: selectedDevice?.alias ?? '' }) }}
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
      <p class="text-xs">{{ t('deviceSync.running.title', { name: selectedDevice?.alias ?? '' }) }}</p>
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
      <Banner tone="success">{{ t('deviceSync.result.done', { name: selectedDevice?.alias ?? '' }) }}</Banner>
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

      <ul v-if="report.warnings.length" class="space-y-1 text-[11px] text-warning">
        <li v-for="(w, i) in report.warnings" :key="i" class="flex items-start gap-1.5">
          <AlertTriangle class="mt-px h-3 w-3 shrink-0" :stroke-width="1.75" />
          <span class="break-all">{{ w }}</span>
        </li>
      </ul>
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
