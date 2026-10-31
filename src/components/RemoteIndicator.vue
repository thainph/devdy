<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'radix-vue'
import { ChevronRight, MonitorSmartphone, Smartphone } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { popoverContent } from '@/components/ui/calendarStyles'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { REMOTE_DOT_CLASS, isControlled, remoteTone } from '@/lib/remoteIndicator'
import { isRemoteMode } from '@/lib/ipc/remote'
import RemoteSessionModal from '@/components/remote/RemoteSessionModal.vue'

/**
 * Remote Control dot (sidebar footer): colour = state, tooltip = summary. The popover
 * shows the relay state, who is connected, the kill switch ("Disconnect and sign out":
 * cuts the controller off and rotates its session token away, so it has to sign in
 * again), "Pair a device…" and a Settings link.
 *
 * `floating`: the same dot pinned to a corner while the sidebar is hidden, shown only
 * while another Devdy drives this one, so that warning is never invisible.
 *
 * Hidden while Remote Control is off, and in remote-desktop windows (every call there
 * goes to the controlled machine: it would show its state and let it kick itself).
 */

const props = defineProps<{ floating?: boolean }>()

const { t, locale } = useI18n()
const route = useRoute()
const router = useRouter()
const store = useRemoteControlStore()
const { toast } = useToast()
const enabled = !isRemoteMode()
const open = ref(false)
useEscapeClose(open)
const busy = ref(false)
const pairOpen = ref(false)
// The run the controller lands on: the one on screen when pairing started, else the
// backend picks the most recent one.
const pairRunId = ref<string | null>(null)

const status = computed(() => store.status)
const tone = computed(() => remoteTone(status.value))
const visible = computed(() => {
  if (!enabled || tone.value === 'off') return false
  return props.floating ? tone.value === 'desktop' : true
})

const desktopSince = computed(() => {
  const v = status.value?.desktop_controlled_since
  return v == null
    ? ''
    : new Date(v * 1000).toLocaleTimeString(locale.value, { hour: '2-digit', minute: '2-digit' })
})

const relayLine = computed(() => {
  const s = status.value
  if (tone.value === 'error') return t('misc.remoteIndicator.agentStopped')
  if (s?.connected) return t('misc.remoteIndicator.relayOnline')
  return s?.reconnecting ? t('misc.remoteIndicator.relayReconnecting') : t('misc.remoteIndicator.relayOffline')
})

const tooltip = computed(() => {
  const s = status.value
  const parts = [`${t('misc.remoteIndicator.title')}: ${relayLine.value}`]
  if (s?.desktop_controlled) parts.push(t('misc.remoteIndicator.desktopSince', { time: desktopSince.value }))
  if (s?.session_authenticated) parts.push(t('misc.remoteIndicator.controller'))
  return parts.join('\n')
})

const label = computed(() =>
  tone.value === 'desktop' ? t('misc.remoteIndicator.controlled') : t('misc.remoteIndicator.short'),
)

async function disconnect() {
  if (busy.value) return
  busy.value = true
  try {
    await store.kickController()
  } catch (e) {
    toast.error(t('misc.remoteDesktopHost.disconnectFailed', { error: String(e) }))
  } finally {
    busy.value = false
  }
}

function pair() {
  open.value = false
  pairRunId.value = typeof route.params.runId === 'string' ? route.params.runId : null
  pairOpen.value = true
}

function openSettings() {
  open.value = false
  pairOpen.value = false
  router.push({ path: '/settings', query: { section: 'remote' } }).catch(() => {})
}

let stopWatch: (() => void) | null = null
onMounted(() => {
  if (enabled) stopWatch = store.watchStatus()
})
onBeforeUnmount(() => stopWatch?.())
</script>

<template>
  <PopoverRoot v-if="visible" v-model:open="open">
    <PopoverTrigger as-child>
      <button
        type="button"
        class="flex items-center gap-1.5 rounded px-1 text-[10px] text-muted-foreground/70 hover:text-foreground"
        :class="floating && 'fixed bottom-3 left-3 z-[90] border border-amber-500/40 bg-background/95 px-2 py-1 shadow-lg backdrop-blur'"
        :title="tooltip"
        :aria-label="tooltip"
      >
        <span class="relative flex h-2 w-2">
          <span
            v-if="isControlled(tone)"
            class="absolute inline-flex h-full w-full rounded-full opacity-60 animate-ping"
            :class="REMOTE_DOT_CLASS[tone]"
          />
          <span class="relative inline-flex h-2 w-2 rounded-full" :class="REMOTE_DOT_CLASS[tone]" />
        </span>
        <span>{{ label }}</span>
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent
        :side="floating ? 'right' : 'top'"
        align="end"
        :side-offset="6"
        :class="[popoverContent, 'w-64 space-y-2.5']"
      >
        <p class="flex items-center gap-1.5 text-xs">
          <span class="font-semibold">{{ t('misc.remoteIndicator.title') }}</span>
          <span class="text-muted-foreground">·</span>
          <span
            class="min-w-0 truncate text-[11px]"
            :class="tone === 'error' ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground'"
          >{{ relayLine }}</span>
        </p>

        <div class="space-y-1 text-[11px]">
          <p v-if="status?.desktop_controlled" class="flex items-center gap-1.5 text-amber-600 dark:text-amber-400">
            <MonitorSmartphone class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
            {{ t('misc.remoteIndicator.desktopSince', { time: desktopSince }) }}
          </p>
          <p v-if="status?.session_authenticated" class="flex items-center gap-1.5 text-emerald-600 dark:text-emerald-400">
            <Smartphone class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
            {{ t('misc.remoteIndicator.controller') }}
          </p>
          <p v-if="!isControlled(tone)" class="text-muted-foreground">{{ t('misc.remoteIndicator.nobody') }}</p>
        </div>

        <Button
          v-if="isControlled(tone)"
          variant="destructive"
          size="sm"
          class="w-full"
          :disabled="busy"
          :title="t('misc.remoteDesktopHost.hint')"
          @click="disconnect"
        >
          {{ t('misc.remoteDesktopHost.disconnect') }}
        </Button>
        <Button v-else size="sm" variant="outline" class="w-full" :disabled="tone === 'error'" @click="pair">
          <Smartphone class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('misc.remoteIndicator.pair') }}
        </Button>

        <button
          type="button"
          class="flex w-full items-center justify-between text-[11px] text-muted-foreground hover:text-foreground"
          @click="openSettings"
        >
          {{ t('misc.remoteIndicator.settings') }}
          <ChevronRight class="h-3 w-3" :stroke-width="1.75" />
        </button>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
  <RemoteSessionModal
    v-if="pairOpen"
    :open="pairOpen"
    :run-id="pairRunId"
    @close="pairOpen = false"
    @open-settings="openSettings"
  />
</template>
