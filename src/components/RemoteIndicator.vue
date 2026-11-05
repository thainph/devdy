<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'radix-vue'
import { AlertTriangle, ArrowDownToLine, ChevronRight, Radio, Smartphone, Unplug } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { popoverContent } from '@/components/ui/calendarStyles'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { useToast } from '@/composables/useToast'
import { useLastSync } from '@/composables/useLastSync'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { REMOTE_DOT_CLASS, isControlled, phoneControlled, remoteTone } from '@/lib/remoteIndicator'
import { nextSetupStep } from '@/lib/remoteDevice'
import RemoteSessionModal from '@/components/remote/RemoteSessionModal.vue'
import DeviceSyncDialog from '@/components/deviceSync/DeviceSyncDialog.vue'

/**
 * Remote Control dot (sidebar footer): colour = state, tooltip = summary. The popover
 * shows the relay state, this machine's name, a clash warning, the one action that fits
 * (turn on / pair a phone / disconnect it), "Sync from another device…", when this
 * machine last synced, and a link to Settings → Devices.
 *
 * `floating`: the bare dot pinned to a corner while the sidebar is hidden, shown only
 * while a Controller (a phone / web) drives this one, so that warning is never
 * invisible.
 *
 * Hidden until a relay is configured.
 */

const props = defineProps<{ floating?: boolean }>()

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const store = useRemoteControlStore()
const { toast } = useToast()
const { label: lastSyncLabel, refresh: refreshLastSync } = useLastSync()
const open = ref(false)
useEscapeClose(open)
const busy = ref(false)
const pairOpen = ref(false)
const syncOpen = ref(false)
// The run the controller lands on: the one on screen when pairing started, else the
// backend picks the most recent one.
const pairRunId = ref<string | null>(null)

const status = computed(() => store.status)
const tone = computed(() => remoteTone(status.value))
const visible = computed(() => {
  if (tone.value === 'hidden') return false
  return props.floating ? isControlled(tone.value) : true
})
/** Off with only the switch left: it can be turned on from here. */
const canEnable = computed(() => nextSetupStep(status.value) === 'enable')
const relayHost = computed(() => {
  try {
    return new URL(status.value?.relay_url ?? '').host
  } catch {
    return ''
  }
})

const stateLine = computed(() => t(`misc.remoteIndicator.state.${tone.value}`))
const conflictLine = computed(() => {
  const c = status.value?.deviceConflict
  if (!c) return ''
  return c.kind === 'alias'
    ? t('misc.remoteIndicator.aliasConflict', { name: c.alias || status.value?.device_alias || '' })
    : t('misc.remoteIndicator.idConflict')
})

const tooltip = computed(() => {
  const parts = [`${t('misc.remoteIndicator.title')}: ${stateLine.value}`]
  if (conflictLine.value) parts.push(conflictLine.value)
  return parts.join('\n')
})

watch(open, (v) => {
  if (v) refreshLastSync()
})

async function act(fn: () => Promise<unknown>, failKey: string) {
  if (busy.value) return
  busy.value = true
  try {
    await fn()
  } catch (e) {
    toast.error(t(failKey, { error: String(e) }))
  } finally {
    busy.value = false
  }
}

function disconnect() {
  void act(() => store.kickController(), 'misc.remoteIndicator.disconnectFailed')
}

function enable() {
  void act(() => store.enable(), 'misc.remoteIndicator.enableFailed')
}

function pair() {
  open.value = false
  pairRunId.value = typeof route.params.runId === 'string' ? route.params.runId : null
  pairOpen.value = true
}

function sync() {
  open.value = false
  syncOpen.value = true
}

function openSettings() {
  open.value = false
  pairOpen.value = false
  router.push({ path: '/settings', query: { section: 'devices' } }).catch(() => {})
}

let stopWatch: (() => void) | null = null
onMounted(() => {
  stopWatch = store.watchStatus()
})
onBeforeUnmount(() => stopWatch?.())
</script>

<template>
  <PopoverRoot v-if="visible" v-model:open="open">
    <PopoverTrigger as-child>
      <button
        type="button"
        class="flex items-center gap-1.5 rounded px-1 text-[10px] text-muted-foreground/70 hover:text-foreground"
        :class="floating && [
          'fixed bottom-3 left-3 z-[90] border bg-background/95 p-1.5 shadow-lg backdrop-blur',
          'border-success/40',
        ]"
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
        <span v-if="!floating">{{ t('misc.remoteIndicator.short') }}</span>
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
            :class="tone === 'error' ? 'text-destructive' : tone === 'online' || tone === 'phone' ? 'text-success' : tone === 'offline' || tone === 'conflict' ? 'text-warning' : 'text-muted-foreground'"
          >{{ stateLine }}</span>
        </p>

        <p v-if="phoneControlled(status)" class="flex items-center gap-1.5 text-[11px] text-success">
          <Smartphone class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
          {{ t('misc.remoteIndicator.controller') }}
        </p>
        <p v-else-if="status?.enabled" class="truncate text-[11px] text-muted-foreground">
          <span class="font-mono text-foreground">{{ status.device_alias || '—' }}</span>
          <template v-if="relayHost"> · {{ relayHost }}</template>
        </p>

        <div v-if="conflictLine" class="flex items-start gap-1.5 text-[11px] text-warning">
          <AlertTriangle class="mt-px h-3.5 w-3.5 shrink-0" :stroke-width="2" />
          <span class="flex-1">{{ conflictLine }}</span>
          <button type="button" class="shrink-0 text-primary hover:underline" @click="openSettings">
            {{ t('misc.remoteIndicator.view') }}
          </button>
        </div>

        <div class="space-y-1.5">
          <Button
            v-if="isControlled(tone)"
            variant="destructive"
            size="sm"
            class="w-full justify-start"
            :disabled="busy"
            :title="t('misc.remoteIndicator.disconnectHint')"
            @click="disconnect"
          >
            <Unplug class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('remote.device.actions.disconnectPhone') }}
          </Button>
          <Button v-else-if="tone === 'off' && canEnable" size="sm" class="w-full justify-start" :disabled="busy" @click="enable">
            <Radio class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('remote.device.actions.enable') }}
          </Button>
          <Button
            v-else-if="tone !== 'off'"
            size="sm"
            variant="outline"
            class="w-full justify-start"
            :disabled="!status?.connected || status?.deviceConflict?.kind === 'device_id'"
            @click="pair"
          >
            <Smartphone class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('remote.device.actions.pair') }}
          </Button>
          <Button size="sm" variant="outline" class="w-full justify-start" @click="sync">
            <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('deviceSync.open') }}…
          </Button>
        </div>
        <p v-if="lastSyncLabel" class="text-[10px] text-muted-foreground/80">{{ lastSyncLabel }}</p>

        <button
          type="button"
          class="flex w-full items-center justify-between border-t border-border/50 pt-2 text-[11px] text-muted-foreground hover:text-foreground"
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
  <Teleport to="body">
    <DeviceSyncDialog v-if="syncOpen" :open="syncOpen" @close="syncOpen = false" />
  </Teleport>
</template>
