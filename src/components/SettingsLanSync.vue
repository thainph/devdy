<script setup lang="ts">
// Settings → "Backup & Sync" → LAN sync: pull the whole configuration of another Devdy machine on
// the same network (same bundle as a settings backup, merged the same way). The machine that has
// the data starts sharing and shows a one-time code; this side picks it from the mDNS list (or
// types its `ip:port`), enters the code and imports. Backend: `src-tauri/src/lan_sync/`.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Download, KeyRound, Loader2, Monitor, Radio, RefreshCw, Square, Upload, Wifi } from 'lucide-vue-next'
import { Button, Card, Input } from '@/components/ui'
import BackupContents from '@/components/BackupContents.vue'
import BackupImportReport from '@/components/BackupImportReport.vue'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'
import {
  formatExportedAt,
  formatShareCode,
  LAN_SHARE_ENDED_EVENT,
  type ImportReport,
  type LanDevice,
  type LanFetchResult,
  type LanShareEnded,
  type LanShareStatus,
} from '@/lib/settingsBackup'

const MANUAL = 'manual'

const { t } = useI18n()
const { toast } = useToast()

// ---- Pull from another device ----
const devices = ref<LanDevice[]>([])
const scanning = ref(false)
const selected = ref<string | null>(null)
const manualEndpoint = ref('')
const code = ref('')
const fetching = ref(false)
const fetched = ref<LanFetchResult | null>(null)
const createMissingDirs = ref(true)
const importing = ref(false)
const report = ref<ImportReport | null>(null)

const codeValid = computed(() => /^\d{6}$/.test(code.value.replace(/[\s-]/g, '')))

const targetEndpoints = computed(() => {
  if (selected.value === MANUAL) {
    const endpoint = manualEndpoint.value.trim()
    return endpoint ? [endpoint] : []
  }
  return devices.value.find(d => d.id === selected.value)?.endpoints ?? []
})

const canFetch = computed(() => !fetching.value && codeValid.value && targetEndpoints.value.length > 0)

async function scan() {
  if (scanning.value) return
  scanning.value = true
  try {
    devices.value = await invoke<LanDevice[]>('lan_sync_discover', { waitMs: 2500 })
    if (selected.value !== MANUAL && !devices.value.some(d => d.id === selected.value)) {
      selected.value = devices.value.length === 1 ? devices.value[0].id : null
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    scanning.value = false
  }
}

async function runFetch() {
  if (!canFetch.value) return
  fetching.value = true
  report.value = null
  try {
    fetched.value = await invoke<LanFetchResult>('lan_sync_fetch', {
      endpoints: targetEndpoints.value,
      code: code.value,
    })
    code.value = ''
  } catch (e) {
    toast.error(String(e))
  } finally {
    fetching.value = false
  }
}

async function runImport() {
  if (!fetched.value || importing.value) return
  importing.value = true
  try {
    report.value = await invoke<ImportReport>('lan_sync_import', { createMissingDirs: createMissingDirs.value })
    fetched.value = null
    toast.success(t('settings.backup.import.done'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    importing.value = false
  }
}

async function cancelImport() {
  fetched.value = null
  await invoke('lan_sync_discard').catch(() => {})
}

// ---- Share this device ----
const share = ref<LanShareStatus | null>(null)
const shareSecrets = ref(false)
const starting = ref(false)
const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null
let unlisten: UnlistenFn | null = null

const remaining = computed(() => {
  if (!share.value) return ''
  const left = Math.max(0, Math.floor((new Date(share.value.expiresAt).getTime() - now.value) / 1000))
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}`
})

async function startShare() {
  if (starting.value) return
  starting.value = true
  try {
    share.value = await invoke<LanShareStatus>('lan_sync_start_share', { includeSecrets: shareSecrets.value })
  } catch (e) {
    toast.error(String(e))
  } finally {
    starting.value = false
  }
}

async function stopShare() {
  try {
    await invoke('lan_sync_stop_share')
  } catch (e) {
    toast.error(String(e))
  }
  share.value = null
}

function onShareEnded(ended: LanShareEnded) {
  share.value = null
  if (ended.reason === 'sent') toast.success(t('settings.lanSync.share.sent', { peer: ended.peer ?? '?' }), 5000)
  else if (ended.reason === 'expired') toast.info(t('settings.lanSync.share.expired'))
  else if (ended.reason === 'tooManyAttempts') toast.error(t('settings.lanSync.share.tooManyAttempts'))
}

onMounted(async () => {
  clock = setInterval(() => { now.value = Date.now() }, 1000)
  unlisten = await listen<LanShareEnded>(LAN_SHARE_ENDED_EVENT, e => onShareEnded(e.payload))
  share.value = await invoke<LanShareStatus | null>('lan_sync_share_status').catch(() => null)
  void scan()
})

onBeforeUnmount(() => {
  if (clock) clearInterval(clock)
  unlisten?.()
})
</script>

<template>
  <Card body-class="p-4 space-y-5">
    <template #header>
      <Wifi class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('settings.lanSync.title') }}</span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('settings.lanSync.help') }}</p>

    <!-- Pull -->
    <section class="space-y-3">
      <div class="flex items-center justify-between gap-2">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.lanSync.pull.title') }}
        </label>
        <Button variant="ghost" size="xs" :disabled="scanning" @click="scan">
          <RefreshCw class="h-3 w-3" :class="{ 'animate-spin': scanning }" :stroke-width="1.75" />
          {{ t('settings.lanSync.pull.rescan') }}
        </Button>
      </div>

      <template v-if="!fetched">
        <div class="space-y-1.5" role="radiogroup" :aria-label="t('settings.lanSync.pull.title')">
          <button
            v-for="device in devices"
            :key="device.id"
            type="button"
            role="radio"
            :aria-checked="selected === device.id"
            class="flex w-full items-center gap-3 rounded-lg border px-3 py-2 text-left transition-colors"
            :class="selected === device.id ? 'border-primary bg-primary/5' : 'border-border/60 bg-muted/20 hover:bg-muted/40'"
            @click="selected = device.id"
          >
            <Monitor class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.5" />
            <span class="min-w-0 flex-1">
              <span class="block truncate text-sm font-medium text-foreground">{{ device.name || device.id }}</span>
              <span class="block truncate font-mono text-[11px] text-muted-foreground">
                {{ device.endpoints.join(', ') }}<template v-if="device.appVersion"> · Devdy {{ device.appVersion }}</template>
              </span>
            </span>
          </button>

          <p v-if="!devices.length" class="rounded-lg border border-dashed border-border/60 px-3 py-2.5 text-[11px] text-muted-foreground">
            <template v-if="scanning">{{ t('settings.lanSync.pull.scanning') }}</template>
            <template v-else>{{ t('settings.lanSync.pull.noDevices') }}</template>
          </p>

          <div
            class="flex w-full items-center gap-2 rounded-lg border px-3 py-2 text-xs transition-colors"
            :class="selected === MANUAL ? 'border-primary bg-primary/5' : 'border-border/60 hover:bg-muted/40'"
          >
            <button
              type="button"
              role="radio"
              :aria-checked="selected === MANUAL"
              class="shrink-0 text-left text-muted-foreground"
              @click="selected = MANUAL"
            >
              {{ t('settings.lanSync.pull.manual') }}
            </button>
            <Input
              v-model="manualEndpoint"
              class="font-mono"
              placeholder="192.168.1.20:52814"
              @focus="selected = MANUAL"
            />
          </div>
        </div>

        <div class="flex items-center gap-2">
          <Input
            v-model="code"
            class="w-32 font-mono tracking-widest"
            inputmode="numeric"
            autocomplete="off"
            maxlength="7"
            :placeholder="t('settings.lanSync.pull.code')"
            @keydown.enter="runFetch"
          />
          <Button size="sm" :disabled="!canFetch" @click="runFetch">
            <component :is="fetching ? Loader2 : Download" class="h-3.5 w-3.5" :class="{ 'animate-spin': fetching }" :stroke-width="1.75" />
            {{ t('settings.lanSync.pull.button') }}
          </Button>
        </div>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.lanSync.pull.codeHint') }}</p>
      </template>

      <div v-else class="space-y-3 rounded-lg border border-border/60 bg-muted/20 p-3">
        <div class="text-[11px] text-muted-foreground space-y-0.5">
          <div class="text-xs font-medium text-foreground">{{ t('settings.lanSync.pull.from', { peer: fetched.peerName }) }}</div>
          <div>
            {{ t('settings.backup.import.exportedAt', { at: formatExportedAt(fetched.info.exportedAt), version: fetched.info.appVersion || '?' }) }}
          </div>
        </div>
        <BackupContents :info="fetched.info" />
        <p class="text-[11px] text-muted-foreground">
          {{ fetched.info.hasSecrets ? t('settings.lanSync.pull.withSecrets') : t('settings.lanSync.pull.withoutSecrets') }}
        </p>

        <label v-if="fetched.info.counts.projects" class="flex items-start gap-2 text-xs">
          <input v-model="createMissingDirs" type="checkbox" class="mt-0.5 accent-primary">
          <span>
            {{ t('settings.backup.import.createMissingDirs') }}
            <span class="block text-[11px] text-muted-foreground">{{ t('settings.backup.import.createMissingDirsHint') }}</span>
          </span>
        </label>

        <p class="text-[11px] text-muted-foreground">{{ t('settings.backup.import.help') }}</p>
        <div class="flex items-center gap-2">
          <Button size="sm" :disabled="importing" @click="runImport">
            <component :is="importing ? Loader2 : Upload" class="h-3.5 w-3.5" :class="{ 'animate-spin': importing }" :stroke-width="1.75" />
            {{ t('settings.backup.import.button') }}
          </Button>
          <Button variant="ghost" size="sm" :disabled="importing" @click="cancelImport">
            {{ t('settings.backup.import.cancel') }}
          </Button>
        </div>
      </div>

      <BackupImportReport v-if="report" :report="report" />
    </section>

    <div class="border-t border-border/60" />

    <!-- Share -->
    <section class="space-y-3">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
        {{ t('settings.lanSync.share.title') }}
      </label>

      <template v-if="!share">
        <p class="text-[11px] text-muted-foreground">{{ t('settings.lanSync.share.help') }}</p>
        <div class="flex items-center justify-between gap-4 rounded-lg border border-border/60 bg-muted/20 px-3.5 py-3">
          <div class="min-w-0">
            <p class="text-sm font-medium text-foreground">{{ t('settings.lanSync.share.includeSecrets') }}</p>
            <p class="mt-0.5 text-[11px] text-muted-foreground">{{ t('settings.lanSync.share.includeSecretsHint') }}</p>
          </div>
          <button
            type="button"
            role="switch"
            :aria-checked="shareSecrets"
            :aria-label="t('settings.lanSync.share.includeSecrets')"
            class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
            :class="shareSecrets ? 'bg-primary' : 'bg-muted'"
            @click="shareSecrets = !shareSecrets"
          >
            <span
              class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform"
              :class="shareSecrets ? 'translate-x-6' : 'translate-x-1'"
            />
          </button>
        </div>
        <Button size="sm" variant="outline" :disabled="starting" @click="startShare">
          <component :is="starting ? Loader2 : Radio" class="h-3.5 w-3.5" :class="{ 'animate-spin': starting }" :stroke-width="1.75" />
          {{ t('settings.lanSync.share.start') }}
        </Button>
      </template>

      <div v-else class="space-y-3 rounded-lg border border-primary/40 bg-primary/5 p-3">
        <div class="flex items-center justify-between gap-3">
          <div class="min-w-0">
            <p class="text-[11px] text-muted-foreground">{{ t('settings.lanSync.share.codeLabel') }}</p>
            <p class="font-mono text-2xl font-semibold tracking-[0.2em] text-foreground tabular-nums">{{ formatShareCode(share.code) }}</p>
          </div>
          <div class="text-right text-[11px] text-muted-foreground">
            <div>{{ t('settings.lanSync.share.expiresIn', { time: remaining }) }}</div>
            <div class="truncate">{{ share.deviceName }}</div>
          </div>
        </div>
        <p class="text-[11px] text-muted-foreground">
          {{ t('settings.lanSync.share.addresses') }}
          <span class="font-mono text-foreground">{{ share.endpoints.join(', ') || '—' }}</span>
        </p>
        <p v-if="!share.advertised" class="text-[11px] text-amber-600 dark:text-amber-400">
          {{ t('settings.lanSync.share.notAdvertised') }}
        </p>
        <p v-if="share.includeSecrets" class="flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
          <KeyRound class="mt-px h-3 w-3 shrink-0" :stroke-width="1.75" />
          {{ t('settings.lanSync.share.secretsWarning') }}
        </p>
        <Button size="sm" variant="outline" @click="stopShare">
          <Square class="h-3 w-3" :stroke-width="1.75" />
          {{ t('settings.lanSync.share.stop') }}
        </Button>
      </div>
    </section>
  </Card>
</template>
