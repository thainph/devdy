<script setup lang="ts">
// Host-side Remote Control settings. Lets the Owner enable the feature and
// review the audit log; the relay, device name and master password it uses are
// in DeviceRelaySettings (shared with Sync). Remote sessions are now
// created per-run via the Remote button inside a run (RemoteSessionModal) —
// there is no global device pairing anymore.
import { onMounted, onUnmounted, ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Card, Badge } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { isRemoteMode } from '@/lib/ipc/remote'
import { openRemoteDesktopWindow } from '@/lib/remoteDesktopWindow'
import { Radio, Wifi, WifiOff, ShieldCheck, RefreshCw, Loader2, Trash2, MonitorSmartphone } from 'lucide-vue-next'

const store = useRemoteControlStore()
const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const clearingAudit = ref(false)

const togglingEnabled = ref(false)
// A remote-desktop window already drives another Devdy; no nesting.
const remoteMode = isRemoteMode()

let pollTimer: ReturnType<typeof setInterval> | null = null

const status = computed(() => store.status)
const enabled = computed(() => status.value?.enabled ?? false)
const running = computed(() => status.value?.running ?? false)
const connected = computed(() => status.value?.connected ?? false)
const reconnecting = computed(() => status.value?.reconnecting ?? false)
// Can't start the agent without a relay to dial — allow turning OFF anytime, but
// only allow turning ON once a Relay URL has been saved.
const canToggle = computed(() => enabled.value || !!status.value?.relay_url?.trim())

async function load() {
  try {
    await store.refreshAll()
  } catch (e) {
    toast.error(String(e))
  }
}

onMounted(() => {
  load()
  // Keep the status + audit trail reasonably fresh while the screen is open
  // (remote sessions come and go out-of-band from another device).
  pollTimer = setInterval(() => {
    store.refreshStatus().catch(() => {})
    store.refreshAudit().catch(() => {})
  }, 5000)
})

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
})

async function openRemoteDesktop() {
  try {
    await openRemoteDesktopWindow()
  } catch (e) {
    toast.error(String(e))
  }
}

async function toggleEnabled() {
  togglingEnabled.value = true
  try {
    if (enabled.value) {
      await store.disable()
      toast.success(t('remote.settings.toast.disabled'))
    } else {
      await store.enable()
      toast.success(t('remote.settings.toast.enabled'))
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    togglingEnabled.value = false
  }
}

async function clearAudit() {
  if (clearingAudit.value) return
  const ok = await confirm({
    title: t('remote.settings.confirmClearAudit.title'),
    message: t('remote.settings.confirmClearAudit.message'),
    confirmLabel: t('remote.settings.confirmClearAudit.confirmLabel'),
    variant: 'destructive',
  })
  if (!ok) return
  clearingAudit.value = true
  try {
    const removed = await store.clearAudit()
    toast.success(
      removed > 0
        ? t('remote.settings.toast.auditCleared', { count: removed })
        : t('remote.settings.toast.auditAlreadyEmpty'),
    )
  } catch (e) {
    toast.error(String(e))
  } finally {
    clearingAudit.value = false
  }
}

function fmtTime(ts: string | null): string {
  if (!ts) return '—'
  const d = new Date(ts)
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleString()
}

function resultTone(r: string): 'success' | 'error' | 'neutral' {
  if (r === 'accepted') return 'success'
  if (r === 'rejected') return 'error'
  return 'neutral'
}
</script>

<template>
  <div class="space-y-6">
    <!-- Status + enable toggle -->
    <Card>
      <template #header>
        <Radio class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1">{{ t('remote.settings.title') }}</h3>
        <Badge v-if="enabled && connected" tone="success" class="gap-1">
          <Wifi class="h-3 w-3" :stroke-width="2" /> {{ t('remote.settings.connected') }}
        </Badge>
        <Badge v-else-if="enabled && reconnecting" tone="warning" class="gap-1">
          <Loader2 class="h-3 w-3 animate-spin" :stroke-width="2" /> {{ t('remote.settings.reconnecting') }}
        </Badge>
        <Badge v-else-if="enabled && running" tone="warning" class="gap-1">
          <WifiOff class="h-3 w-3" :stroke-width="2" /> {{ t('remote.settings.waiting') }}
        </Badge>
        <Badge v-else tone="neutral">{{ t('remote.settings.off') }}</Badge>
      </template>
      <div class="p-4 space-y-4">
        <p class="text-xs text-muted-foreground">
          {{ t('remote.settings.intro') }}
        </p>

        <!-- Master on/off switch. This is the primary control for the feature,
             so it reads as a switch rather than competing with "Save settings". -->
        <div
          class="flex items-center justify-between gap-4 rounded-lg border border-border/60 bg-muted/20 px-3.5 py-3"
        >
          <div class="min-w-0">
            <p class="text-sm font-medium text-foreground">{{ t('remote.settings.enable') }}</p>
            <p class="mt-0.5 text-[11px] text-muted-foreground">
              <template v-if="enabled && connected">{{ t('remote.settings.state.connected') }}</template>
              <template v-else-if="enabled">{{ t('remote.settings.state.running') }}</template>
              <template v-else-if="!canToggle">{{ t('remote.settings.state.needsRelay') }}</template>
              <template v-else>{{ t('remote.settings.state.off') }}</template>
            </p>
          </div>
          <button
            type="button"
            role="switch"
            :aria-checked="enabled"
            :disabled="togglingEnabled || !canToggle"
            class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:cursor-not-allowed disabled:opacity-50"
            :class="enabled ? 'bg-primary' : 'bg-muted'"
            @click="toggleEnabled"
          >
            <span
              class="inline-flex h-4 w-4 transform items-center justify-center rounded-full bg-white shadow transition-transform"
              :class="enabled ? 'translate-x-6' : 'translate-x-1'"
            >
              <Loader2
                v-if="togglingEnabled"
                class="h-3 w-3 animate-spin text-primary"
                :stroke-width="2.5"
              />
            </span>
          </button>
        </div>

        <p class="text-[11px] text-muted-foreground/70 border-t border-border/50 pt-3">
          <i18n-t keypath="remote.settings.perRunHint" tag="span">
            <template #remote>
              <span class="font-medium text-foreground/80">{{ t('remote.settings.remote') }}</span>
            </template>
            <template #masterPassword>
              <span class="font-medium text-foreground/80">{{ t('remote.settings.masterPasswordInline') }}</span>
            </template>
          </i18n-t>
        </p>

        <!-- Remote desktop: drive another Devdy from this one. -->
        <div
          v-if="!remoteMode"
          class="flex items-center justify-between gap-4 border-t border-border/50 pt-3"
        >
          <div class="min-w-0">
            <p class="text-sm font-medium text-foreground">{{ t('remote.settings.controlOther') }}</p>
            <p class="mt-0.5 text-[11px] text-muted-foreground">{{ t('remote.settings.controlOtherHint') }}</p>
          </div>
          <Button variant="outline" class="shrink-0" @click="openRemoteDesktop">
            <MonitorSmartphone class="h-4 w-4" :stroke-width="1.75" />
            {{ t('remote.settings.controlOtherOpen') }}
          </Button>
        </div>
      </div>
    </Card>

    <!-- Audit log -->
    <Card>
      <template #header>
        <ShieldCheck class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1">{{ t('remote.settings.auditLog') }}</h3>
        <div class="flex items-center gap-1">
          <button
            class="p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent transition-colors"
            :title="t('common.refresh')"
            @click="store.refreshAudit().catch(() => {})"
          >
            <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
          <button
            class="p-1 rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            :title="t('remote.settings.clearLog')"
            :disabled="clearingAudit || !store.audit.length"
            @click="clearAudit"
          >
            <Loader2 v-if="clearingAudit" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
            <Trash2 v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
        </div>
      </template>
      <div v-if="store.audit.length" class="max-h-80 overflow-auto">
        <table class="w-full text-xs">
          <thead class="sticky top-0 bg-muted/40 text-muted-foreground">
            <tr>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.settings.table.time') }}</th>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.settings.table.action') }}</th>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.settings.table.result') }}</th>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.settings.table.run') }}</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-border/40">
            <tr v-for="a in store.audit" :key="a.id">
              <td class="px-3 py-1.5 whitespace-nowrap text-muted-foreground">{{ fmtTime(a.ts) }}</td>
              <td class="px-3 py-1.5 font-mono">{{ a.action }}</td>
              <td class="px-3 py-1.5">
                <Badge :tone="resultTone(a.result)" size="xs">{{ a.result }}</Badge>
                <span v-if="a.reason" class="ml-1 text-muted-foreground/70">{{ a.reason }}</span>
              </td>
              <td class="px-3 py-1.5 font-mono text-muted-foreground/70">
                {{ a.run_id ? a.run_id.slice(0, 8) : '—' }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-else class="px-4 py-6 text-center text-xs text-muted-foreground">
        {{ t('remote.settings.noActivity') }}
      </div>
    </Card>
  </div>
</template>
