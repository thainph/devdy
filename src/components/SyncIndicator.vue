<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'radix-vue'
import { ChevronRight, Loader2, RefreshCw } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { popoverContent } from '@/components/ui/calendarStyles'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { useSyncStore } from '@/stores/sync'
import { SYNC_DOT_CLASS, ageParts, syncErrorCode, syncStateKey, syncTone } from '@/lib/syncCore'
import { isRemoteMode } from '@/lib/ipc/remote'

/**
 * Sidebar-footer dot for sync: colour = state, tooltip = state + last sync + pending +
 * refused (parked) changes + the last error.
 * Hidden while this device is not in a sync group and in remote-desktop windows (it would show
 * the other machine's state). Click opens a small popover (source device + online, last sync,
 * "Sync now", settings link); while a Sync runs it shows a spinner + % and reopens the Sync dialog.
 */

const { t } = useI18n()
const router = useRouter()
const store = useSyncStore()
const enabled = !isRemoteMode()
const open = ref(false)
useEscapeClose(open)

const status = computed(() => store.status)
const tone = computed(() => syncTone(status.value, store.mergePending, store.busy))
const paired = computed(() => status.value.state !== 'disabled' && !store.mergePending)
const source = computed(() => store.source)
const sourceOnline = computed(() => store.isOnline(source.value))

function ago(ms: number | null | undefined): string {
  const { unit, n } = ageParts(ms)
  return t(`settings.sync.ago.${unit}`, { n })
}

const tooltip = computed(() => {
  const s = status.value
  if (store.busy) return t('settings.sync.indicator.syncing', { n: store.percent })
  const parts = [
    `${t('settings.sync.title')}: ${t(`settings.sync.state.${syncStateKey(s, store.mergePending)}`)}`,
    t('settings.sync.indicator.lastSync', { ago: ago(store.lastRun?.at) }),
  ]
  if (s.pending) parts.push(`${t('settings.sync.status.pending')}: ${s.pending}`)
  if (s.parked) parts.push(t('settings.sync.parked.tooltip', { n: s.parked }))
  if (s.last_error && s.state !== 'online') {
    const code = syncErrorCode(s.last_error)
    parts.push(code ? t(`settings.sync.errors.${code}`) : s.last_error)
  }
  return parts.join('\n')
})

const syncingLabel = computed(() =>
  store.run.stage === 'applying' ? `${store.percent}%` : t('settings.sync.short'),
)

function openSettings() {
  open.value = false
  router.push({ path: '/settings', query: { section: 'sync' } }).catch(() => {})
}

function syncNow() {
  open.value = false
  void store.syncFrom()
}

// While a Sync runs the click reopens its dialog; before the merge it goes to Settings. Otherwise
// load a fresh device list / source / online state each time the popover opens.
watch(open, (v) => {
  if (!v) return
  if (store.busy) {
    open.value = false
    store.dialogOpen = true
  } else if (!paired.value) {
    openSettings()
  } else {
    void store.loadForSync()
  }
})

onMounted(() => {
  if (enabled) store.start().catch(() => {})
})
</script>

<template>
  <PopoverRoot v-if="enabled && (status.state !== 'disabled' || store.mergePending)" v-model:open="open">
    <PopoverTrigger as-child>
      <button
        type="button"
        class="flex items-center gap-1.5 rounded px-1 text-[10px] text-muted-foreground/70 hover:text-foreground"
        :title="tooltip"
        :aria-label="tooltip"
      >
        <Loader2 v-if="store.busy" class="h-2.5 w-2.5 animate-spin text-sky-500" :stroke-width="2" />
        <span v-else class="relative flex h-2 w-2">
          <span
            v-if="tone === 'syncing'"
            class="absolute inline-flex h-full w-full rounded-full opacity-60 animate-ping"
            :class="SYNC_DOT_CLASS[tone]"
          />
          <span class="relative inline-flex h-2 w-2 rounded-full" :class="SYNC_DOT_CLASS[tone]" />
        </span>
        <span class="tabular-nums">{{ store.busy ? syncingLabel : t('settings.sync.short') }}</span>
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent side="top" align="end" :side-offset="6" :class="[popoverContent, 'w-60 space-y-2.5']">
        <p class="flex items-center gap-1.5 text-xs">
          <span class="font-semibold">{{ t('settings.sync.title') }}</span>
          <span class="text-muted-foreground">·</span>
          <template v-if="source">
            <span class="min-w-0 truncate">{{ store.deviceName(source) }}</span>
            <span class="h-2 w-2 shrink-0 rounded-full" :class="sourceOnline ? 'bg-emerald-500' : 'bg-muted-foreground/40'" />
            <span class="text-[11px] text-muted-foreground">
              {{ sourceOnline ? t('settings.sync.ondemand.online') : t('settings.sync.ondemand.offline') }}
            </span>
          </template>
          <span v-else class="text-muted-foreground">{{ t('settings.sync.indicator.noSource') }}</span>
        </p>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.indicator.lastSync', { ago: ago(store.lastRun?.at) }) }}</p>
        <p v-if="source && !sourceOnline" class="text-[11px] text-amber-600 dark:text-amber-400">
          {{ t('settings.sync.ondemand.offlineHelp', { name: store.deviceName(source) }) }}
        </p>
        <Button size="sm" class="w-full" :disabled="!source || !sourceOnline" @click="syncNow">
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('settings.sync.indicator.syncNow') }}
        </Button>
        <button
          type="button"
          class="flex w-full items-center justify-between text-[11px] text-muted-foreground hover:text-foreground"
          @click="openSettings"
        >
          {{ t('settings.sync.indicator.settings') }}
          <ChevronRight class="h-3 w-3" :stroke-width="1.75" />
        </button>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
