<script setup lang="ts">
/**
 * Fixed pill on the remote-desktop window: which Devdy is being controlled, the
 * live connection state, and the ways out — reconnect after an error, sign in
 * again when the stored token stops working, or sign out.
 *
 * A rejected token does not always surface as `auth === 'failed'` (the client
 * keeps retrying a session-mode connection), so "Sign in again" is also
 * offered after STUCK_MS without a connection.
 */
import { computed, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { LogOut, MonitorSmartphone, RefreshCw } from 'lucide-vue-next'
import type { RemoteIpcClient } from '@/lib/ipc/remote'
import { clearRemoteDesktopSession } from '@/lib/remoteDesktopSession'

const props = defineProps<{ client: RemoteIpcClient; device: string }>()

const { t } = useI18n()

const STUCK_MS = 60_000

const status = computed(() => props.client.state.status.value)
const auth = computed(() => props.client.state.auth.value)
const connected = computed(() => status.value === 'connected' && auth.value === 'authenticated')
const failed = computed(() => auth.value === 'failed' || status.value === 'error')

// Continuously not connected for STUCK_MS ⇒ probably a dead token.
const stuck = ref(false)
let stuckTimer: ReturnType<typeof setTimeout> | null = null
watch(
  connected,
  (ok) => {
    if (stuckTimer) clearTimeout(stuckTimer)
    stuckTimer = null
    stuck.value = false
    if (!ok) stuckTimer = setTimeout(() => (stuck.value = true), STUCK_MS)
  },
  { immediate: true },
)
onUnmounted(() => {
  if (stuckTimer) clearTimeout(stuckTimer)
})

const label = computed(() => (props.device ? props.device : t('remote.desktop.banner.unnamed')))

const stateText = computed(() => {
  if (auth.value === 'failed') return t('remote.desktop.banner.authFailed')
  if (connected.value) return t('remote.desktop.banner.connected')
  switch (status.value) {
    case 'error':
      return t('remote.desktop.banner.error')
    case 'disconnected':
      return t('remote.desktop.banner.disconnected')
    case 'reconnecting':
      return t('remote.desktop.banner.reconnecting')
    default:
      return t('remote.desktop.banner.connecting')
  }
})

const dotClass = computed(() =>
  connected.value ? 'bg-emerald-500' : failed.value ? 'bg-destructive' : 'bg-amber-500 animate-pulse',
)

const canReconnect = computed(() => status.value === 'error' || status.value === 'disconnected')
const needsSignIn = computed(() => failed.value || stuck.value)

function reconnect(): void {
  // A fresh boot re-runs startRemoteIpc with the stored token.
  location.reload()
}

function signOut(): void {
  props.client.close()
  clearRemoteDesktopSession()
  location.reload()
}
</script>

<template>
  <div
    class="fixed bottom-3 left-1/2 z-[1000] -translate-x-1/2 flex items-center gap-2 rounded-full border border-border bg-background/95 px-3 py-1.5 text-xs shadow-lg backdrop-blur"
    role="status"
  >
    <MonitorSmartphone class="h-3.5 w-3.5 text-primary" :stroke-width="2" />
    <span class="font-medium">{{ t('remote.desktop.banner.controlling', { device: label }) }}</span>
    <span class="h-2 w-2 rounded-full" :class="dotClass" />
    <span class="text-muted-foreground">{{ stateText }}</span>
    <button
      v-if="canReconnect && !needsSignIn"
      type="button"
      class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 hover:bg-muted"
      @click="reconnect"
    >
      <RefreshCw class="h-3 w-3" :stroke-width="2" /> {{ t('remote.desktop.banner.reconnect') }}
    </button>
    <button
      v-if="needsSignIn"
      type="button"
      class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-primary hover:bg-muted"
      @click="signOut"
    >
      {{ t('remote.desktop.banner.signInAgain') }}
    </button>
    <button
      v-else
      type="button"
      class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
      @click="signOut"
    >
      <LogOut class="h-3 w-3" :stroke-width="2" /> {{ t('remote.desktop.banner.signOut') }}
    </button>
  </div>
</template>
