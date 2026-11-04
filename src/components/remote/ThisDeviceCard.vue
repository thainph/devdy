<script setup lang="ts">
// Settings → Devices, top card: this machine's name, one status badge and the one action
// that matters in that state (set up / turn on / pair a phone / disconnect it / retry /
// resolve a name or device id clash), plus the on/off switch.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  ArrowDown, ArrowDownToLine, Laptop, Loader2, Monitor, Radio, RefreshCcwDot, RotateCcw, Smartphone, Unplug,
} from 'lucide-vue-next'
import { Badge, Banner, Button, Card, ToggleSwitch } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { deviceState, errorReason, nextSetupStep, setupDone, suggestAlias, SETUP_STEPS } from '@/lib/remoteDevice'
import DeviceNameForm from './DeviceNameForm.vue'

const emit = defineEmits<{ setup: []; pair: []; sync: []; editRelay: [] }>()

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const store = useRemoteControlStore()

const status = computed(() => store.status)
const state = computed(() => deviceState(status.value))
const done = computed(() => setupDone(status.value))
const doneCount = computed(() => SETUP_STEPS.filter(k => done.value[k]).length)
const alias = computed(() => status.value?.device_alias || '')
const relayHost = computed(() => {
  try {
    return new URL(status.value?.relay_url ?? '').host
  } catch {
    return status.value?.relay_url ?? ''
  }
})
/** Off with only the switch left, or on: the switch can be used. */
const canToggle = computed(() => !!status.value?.enabled || nextSetupStep(status.value) === 'enable')
const canSync = computed(() => done.value.relay)

const BADGE = {
  setup: 'neutral', off: 'neutral', connecting: 'warning', error: 'error',
  ready: 'success', phone: 'success', aliasConflict: 'warning', idConflict: 'error',
} as const

const busy = ref(false)
const renaming = ref(false)

async function run(fn: () => Promise<unknown>, ok?: string) {
  if (busy.value) return
  busy.value = true
  try {
    await fn()
    if (ok) toast.success(ok)
  } catch (e) {
    toast.error(String(e))
  } finally {
    busy.value = false
  }
}

function toggle() {
  if (status.value?.enabled) void run(() => store.disable(), t('remote.device.toast.disabled'))
  else void run(() => store.enable(), t('remote.device.toast.enabled'))
}

function retry() {
  void run(async () => {
    await store.disable()
    await store.enable()
  })
}

function disconnectPhone() {
  void run(() => store.kickController(), t('remote.device.toast.phoneDisconnected'))
}

async function newDeviceId() {
  const ok = await confirm({
    title: t('remote.conflict.confirmTitle'),
    message: t('remote.conflict.confirmMessage'),
    confirmLabel: t('remote.conflict.newId'),
    variant: 'primary',
  })
  if (!ok) return
  await run(() => store.regenerateDeviceId(), t('remote.conflict.newIdDone'))
}

function renamed() {
  renaming.value = false
  toast.success(t('remote.name.saved'))
}
</script>

<template>
  <Card>
    <template #header>
      <Monitor class="h-4 w-4 text-primary" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('remote.device.title') }}</h3>
      <Badge :tone="BADGE[state]" class="gap-1">
        <Loader2 v-if="state === 'connecting'" class="h-3 w-3 animate-spin" :stroke-width="2" />
        <span v-else-if="state === 'ready' || state === 'phone'" class="relative flex h-1.5 w-1.5">
          <span v-if="state === 'phone'" class="absolute inline-flex h-full w-full animate-ping rounded-full bg-white opacity-75" />
          <span class="relative h-1.5 w-1.5 rounded-full bg-white" />
        </span>
        {{ t(`remote.device.state.${state}`) }}
      </Badge>
    </template>

    <div class="space-y-3 p-4">
      <div class="flex items-center gap-4">
        <div
          class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg"
          :class="state === 'ready' || state === 'phone' ? 'bg-success/10 text-success' : 'bg-muted text-muted-foreground'"
        >
          <Smartphone v-if="state === 'phone'" class="h-5 w-5" :stroke-width="1.75" />
          <Laptop v-else class="h-5 w-5" :stroke-width="1.75" />
        </div>
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-semibold font-mono">{{ alias || t('remote.device.unnamed') }}</p>
          <p class="text-[11px] text-muted-foreground">
            <template v-if="state === 'setup'">{{ t('remote.device.line.setup', { done: doneCount, total: SETUP_STEPS.length }) }}</template>
            <template v-else-if="state === 'off'">{{ t('remote.device.line.off') }}</template>
            <template v-else-if="state === 'connecting'">{{ t('remote.device.line.connecting', { host: relayHost }) }}</template>
            <template v-else-if="state === 'phone'">{{ t('remote.device.line.phone') }}</template>
            <template v-else-if="state === 'idConflict'">{{ t('remote.device.line.idConflict') }}</template>
            <template v-else-if="state === 'error'">{{ t('remote.device.line.error', { host: relayHost }) }}</template>
            <template v-else>{{ t('remote.device.line.ready', { host: relayHost }) }}</template>
          </p>
          <div v-if="state === 'setup'" class="mt-2 h-1 w-56 overflow-hidden rounded-full bg-muted">
            <div class="h-full bg-primary transition-all" :style="{ width: `${(doneCount / SETUP_STEPS.length) * 100}%` }" />
          </div>
        </div>
        <ToggleSwitch
          size="md"
          :checked="!!status?.enabled"
          :disabled="!canToggle"
          :loading="busy"
          :label="t('remote.device.toggle')"
          @toggle="toggle"
        />
      </div>

      <!-- Why it is not connected -->
      <Banner v-if="state === 'error'" tone="destructive">{{ t(`remote.device.errors.${errorReason(status)}`) }}</Banner>

      <!-- Another running Devdy has this device id: the relay refuses this one -->
      <Banner v-if="state === 'idConflict'" tone="destructive">
        {{ t('remote.conflict.deviceId') }}
      </Banner>

      <!-- Another online device uses this name -->
      <template v-if="status?.deviceConflict?.kind === 'alias'">
        <Banner tone="warning">
          {{ t('remote.conflict.alias', { name: status.deviceConflict.alias || alias }) }}
          <template v-if="!renaming" #actions>
            <Button variant="outline" size="xs" @click="renaming = true">{{ t('remote.conflict.rename') }}</Button>
          </template>
        </Banner>
        <div v-if="renaming" class="rounded-lg border border-border/60 bg-muted/20 p-3">
          <DeviceNameForm
            :initial="suggestAlias(alias)"
            :submit-label="t('remote.conflict.rename')"
            cancelable
            @saved="renamed"
            @cancel="renaming = false"
          />
        </div>
      </template>

      <div class="flex flex-wrap items-center gap-2">
        <template v-if="state === 'setup'">
          <Button size="sm" @click="emit('setup')">
            <ArrowDown class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('remote.device.actions.continueSetup') }}
          </Button>
        </template>
        <template v-else-if="state === 'off'">
          <Button size="sm" :disabled="busy" @click="toggle">
            <Radio class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('remote.device.actions.enable') }}
          </Button>
        </template>
        <template v-else-if="state === 'error'">
          <Button size="sm" :disabled="busy" @click="retry">
            <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('remote.device.actions.retry') }}
          </Button>
          <Button variant="outline" size="sm" @click="emit('editRelay')">{{ t('remote.device.actions.editRelay') }}</Button>
        </template>
        <template v-else-if="state === 'idConflict'">
          <Button size="sm" :disabled="busy" @click="newDeviceId">
            <RefreshCcwDot class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('remote.conflict.newId') }}
          </Button>
        </template>
        <template v-else-if="state === 'phone'">
          <Button variant="destructive" size="sm" :disabled="busy" @click="disconnectPhone">
            <Unplug class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('remote.device.actions.disconnectPhone') }}
          </Button>
          <span class="text-[11px] text-muted-foreground">{{ t('remote.device.disconnectHint') }}</span>
        </template>
        <template v-else>
          <!-- connecting / ready / aliasConflict -->
          <Button size="sm" :disabled="state === 'connecting'" @click="emit('pair')">
            <Smartphone class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('remote.device.actions.pair') }}
          </Button>
        </template>
        <Button v-if="state !== 'setup' || canSync" variant="outline" size="sm" :disabled="!canSync" class="ml-auto" @click="emit('sync')">
          <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('deviceSync.open') }}…
        </Button>
      </div>
    </div>
  </Card>
</template>
