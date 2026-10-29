<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { MonitorSmartphone } from 'lucide-vue-next'
import Button from '@/components/ui/Button.vue'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { useToast } from '@/composables/useToast'
import { isRemoteMode } from '@/lib/ipc/remote'

/**
 * Persistent "being controlled from another Devdy" pill with a kill switch:
 * "Disconnect and sign out" cuts the controller off and rotates its session
 * token away, so it cannot re-join until it signs in again (master password /
 * pairing code). Changing the master password keeps it out for good.
 *
 * Shown on the controlled host (B) while a remote-desktop controller is attached
 * over IPC. Kept outside the sidebar so hiding the sidebar never hides it. Driven
 * by the `remote://desktop-attached|detached` events, with `remote_status` on
 * mount (and the store's regular refreshes) as the fallback.
 *
 * Not rendered in a remote-desktop window (A): there every call goes to B, so it
 * would report B's state and let the controller kick itself.
 */

const { t, locale } = useI18n()
const store = useRemoteControlStore()
const { toast } = useToast()

const enabled = !isRemoteMode()
const since = ref<number | null>(null)
const busy = ref(false)
const unlisteners: UnlistenFn[] = []

// Keep in sync with any status refresh (ActiveRemoteDock polls the same store).
watch(
  () => store.status?.desktop_controlled_since,
  (v) => {
    since.value = v ?? null
  },
)

const sinceLabel = computed(() =>
  since.value == null
    ? ''
    : new Date(since.value * 1000).toLocaleTimeString(locale.value, { hour: '2-digit', minute: '2-digit' }),
)

async function disconnect() {
  if (busy.value) return
  busy.value = true
  try {
    await store.kickController()
    // The detached event clears the pill; drop it now in case it raced.
    since.value = null
  } catch (e) {
    toast.error(t('remoteDesktopHost.disconnectFailed', { error: String(e) }))
  } finally {
    busy.value = false
  }
}

onMounted(async () => {
  if (!enabled) return
  store.refreshStatus().catch(() => {})
  unlisteners.push(
    await listen<{ since_unix: number }>('remote://desktop-attached', (e) => {
      since.value = e.payload.since_unix
    }),
    await listen('remote://desktop-detached', () => {
      since.value = null
    }),
  )
})

onBeforeUnmount(() => {
  for (const un of unlisteners) {
    try {
      un()
    } catch {
      // ignore
    }
  }
})
</script>

<template>
  <div
    v-if="enabled && since != null"
    class="fixed bottom-4 left-4 z-[90] flex max-w-[calc(100vw-2rem)] items-center gap-2.5 rounded-lg border border-amber-500/40 bg-background/95 py-1.5 pl-3 pr-1.5 shadow-lg backdrop-blur"
    role="status"
    :title="t('remoteDesktopHost.hint')"
  >
    <span class="relative flex h-2 w-2 shrink-0">
      <span class="absolute inline-flex h-full w-full rounded-full bg-amber-500 opacity-60 animate-ping" />
      <span class="relative inline-flex h-2 w-2 rounded-full bg-amber-500" />
    </span>
    <MonitorSmartphone class="h-3.5 w-3.5 shrink-0 text-amber-600 dark:text-amber-400" :stroke-width="2" />
    <span class="min-w-0 truncate text-xs">
      <span class="font-medium text-foreground">{{ t('remoteDesktopHost.title') }}</span>
      <span class="text-muted-foreground"> · {{ t('remoteDesktopHost.since', { time: sinceLabel }) }}</span>
    </span>
    <Button variant="destructive" size="xs" :disabled="busy" @click="disconnect">
      {{ t('remoteDesktopHost.disconnect') }}
    </Button>
  </div>
</template>
