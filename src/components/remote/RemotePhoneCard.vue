<script setup lang="ts">
// Settings → Devices, "Phone" card: whether a phone is driving this machine, which events
// Web Push notifies about (docs/remote-mobile-push-spec.md) and the phones that subscribed
// from the controller's Inbox (they can only be removed here).
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Send, Smartphone, Trash2 } from 'lucide-vue-next'
import { Button, Card, ToggleSwitch } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'
import { useRemoteControlStore } from '@/stores/remoteControl'

interface PushEvents {
  pending: boolean
  done: boolean
  failed: boolean
  focus: boolean
}
interface PushDevice {
  id: string
  label: string
  created_at: string
  last_ok_at: string | null
}
interface PushOverview {
  devices: PushDevice[]
  events: PushEvents
}

const KINDS = ['pending', 'done', 'failed', 'focus'] as const

const emit = defineEmits<{ pair: [] }>()

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const store = useRemoteControlStore()

const enabled = computed(() => !!store.status?.enabled)
const phone = computed(() => !!store.status?.session_authenticated)
const devices = ref<PushDevice[]>([])
const events = ref<PushEvents>({ pending: true, done: true, failed: true, focus: true })
const loaded = ref(false)
const busyId = ref<string | null>(null)
const kicking = ref(false)
let pollTimer: ReturnType<typeof setInterval> | null = null

async function load() {
  try {
    const o = await invoke<PushOverview>('remote_push_overview')
    devices.value = o.devices
    events.value = o.events
    loaded.value = true
  } catch (e) {
    toast.error(String(e))
  }
}

onMounted(() => {
  load()
  // Phones subscribe out-of-band; keep the list current while open.
  pollTimer = setInterval(() => load(), 10_000)
})
onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
})

async function toggleKind(kind: (typeof KINDS)[number]) {
  const next = { ...events.value, [kind]: !events.value[kind] }
  const prev = events.value
  events.value = next
  try {
    await invoke('remote_push_set_events', { events: next })
  } catch (e) {
    events.value = prev
    toast.error(String(e))
  }
}

async function test(d: PushDevice) {
  busyId.value = d.id
  try {
    await invoke('remote_push_test', { id: d.id })
    toast.success(t('remote.push.toast.testSent', { label: d.label || t('remote.push.unnamed') }))
  } catch (e) {
    const reason = String(e)
    toast.error(
      reason === 'not_found'
        ? t('remote.push.toast.gone')
        : t('remote.push.toast.testFailed', { reason }),
    )
  } finally {
    busyId.value = null
    load()
  }
}

async function remove(d: PushDevice) {
  const ok = await confirm({
    title: t('remote.push.removeTitle'),
    message: t('remote.push.removeMessage', { label: d.label || t('remote.push.unnamed') }),
    confirmLabel: t('remote.push.remove'),
    variant: 'destructive',
  })
  if (!ok) return
  busyId.value = d.id
  try {
    await invoke('remote_push_remove', { id: d.id })
    devices.value = devices.value.filter((x) => x.id !== d.id)
  } catch (e) {
    toast.error(String(e))
  } finally {
    busyId.value = null
  }
}

async function disconnect() {
  kicking.value = true
  try {
    await store.kickController()
    toast.success(t('remote.device.toast.phoneDisconnected'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    kicking.value = false
  }
}

function fmtTime(ts: string | null): string {
  if (!ts) return t('remote.push.never')
  const d = new Date(ts)
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleString()
}
</script>

<template>
  <Card>
    <template #header>
      <Smartphone class="h-4 w-4 text-primary" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('remote.phone.title') }}</h3>
    </template>
    <div class="p-4 space-y-4">
      <div class="flex items-center gap-3">
        <p class="flex-1 text-xs" :class="phone ? 'text-success' : 'text-muted-foreground'">
          <template v-if="phone">{{ t('remote.phone.controlling') }}</template>
          <template v-else-if="enabled">{{ t('remote.phone.none') }}</template>
          <template v-else>{{ t('remote.phone.off') }}</template>
        </p>
        <Button v-if="phone" variant="destructive" size="xs" :disabled="kicking" @click="disconnect">
          {{ t('remote.device.actions.disconnectPhone') }}
        </Button>
        <Button v-else-if="enabled" variant="outline" size="xs" @click="emit('pair')">
          {{ t('remote.device.actions.pair') }}
        </Button>
      </div>

      <div class="border-t border-border/50 pt-3">
        <p class="mb-0.5 text-xs font-medium text-foreground">{{ t('remote.push.title') }}</p>
        <p class="mb-2.5 text-[11px] text-muted-foreground">{{ t('remote.push.intro') }}</p>
        <div class="grid grid-cols-2 gap-x-6 gap-y-2.5">
          <div v-for="k in KINDS" :key="k" class="flex items-center justify-between gap-3">
            <div class="min-w-0">
              <p class="text-xs text-foreground">{{ t(`remote.push.kinds.${k}`) }}</p>
              <p class="mt-0.5 text-[11px] text-muted-foreground">{{ t(`remote.push.kindHints.${k}`) }}</p>
            </div>
            <ToggleSwitch
              size="md"
              :checked="events[k]"
              :label="t(`remote.push.kinds.${k}`)"
              :disabled="!loaded"
              @toggle="toggleKind(k)"
            />
          </div>
        </div>
      </div>

      <div class="border-t border-border/50 pt-3">
        <p class="mb-2 text-xs font-medium text-foreground">{{ t('remote.push.devices') }}</p>
        <p v-if="loaded && !devices.length" class="text-[11px] text-muted-foreground">
          {{ t('remote.push.noDevices') }}
        </p>
        <ul v-else class="divide-y divide-border/50 rounded-lg border border-border/60">
          <li v-for="d in devices" :key="d.id" class="flex items-center gap-3 px-3 py-2">
            <Smartphone class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-xs font-medium text-foreground">{{ d.label || t('remote.push.unnamed') }}</p>
              <p class="text-[11px] text-muted-foreground">
                {{ t('remote.push.added', { at: fmtTime(d.created_at) }) }} ·
                {{ t('remote.push.lastOk', { at: fmtTime(d.last_ok_at) }) }}
              </p>
            </div>
            <Button
              variant="ghost"
              size="icon-sm"
              :title="t('remote.push.test')"
              :aria-label="t('remote.push.test')"
              :disabled="busyId === d.id"
              @click="test(d)"
            >
              <Loader2 v-if="busyId === d.id" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
              <Send v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
            <Button
              variant="destructive-ghost"
              size="icon-sm"
              :title="t('remote.push.remove')"
              :aria-label="t('remote.push.remove')"
              :disabled="busyId === d.id"
              @click="remove(d)"
            >
              <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </li>
        </ul>
      </div>
    </div>
  </Card>
</template>
