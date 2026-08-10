<script setup lang="ts">
// Host-side Remote Control settings. Lets the Owner enable the feature,
// configure the relay, and review the audit log. Remote sessions are now
// created per-run via the Remote button inside a run (RemoteSessionModal) —
// there is no global device pairing anymore.
import { onMounted, onUnmounted, ref, computed } from 'vue'
import { Button, Input, Card, Badge } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { Radio, Wifi, WifiOff, ShieldCheck, RefreshCw, Loader2, Trash2 } from 'lucide-vue-next'

const store = useRemoteControlStore()
const { toast } = useToast()
const { confirm } = useConfirm()
const clearingAudit = ref(false)

// --- Config form (relay URL + auth token) ---
const relayUrl = ref('')
const authToken = ref('')
const savingConfig = ref(false)
const togglingEnabled = ref(false)
const masterPassword = ref('')
const savingMasterPw = ref(false)

let pollTimer: ReturnType<typeof setInterval> | null = null

const status = computed(() => store.status)
const enabled = computed(() => status.value?.enabled ?? false)
const running = computed(() => status.value?.running ?? false)
const connected = computed(() => status.value?.connected ?? false)
// Can't start the agent without a relay to dial — allow turning OFF anytime, but
// only allow turning ON once a Relay URL has been saved.
const canToggle = computed(() => enabled.value || !!status.value?.relay_url?.trim())

async function load() {
  try {
    await store.refreshAll()
    relayUrl.value = store.status?.relay_url ?? ''
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

async function saveConfig() {
  const url = relayUrl.value.trim()
  if (!url) {
    toast.error('Relay URL is required')
    return
  }
  savingConfig.value = true
  try {
    await store.setConfig(url, authToken.value)
    authToken.value = '' // never keep the token around in the field
    // A non-empty field sets/updates the master password; blank leaves it
    // untouched (same "blank keeps current" convention as the auth token).
    // Clearing is done explicitly via the Clear link.
    if (masterPassword.value.trim()) {
      await store.setMasterPassword(masterPassword.value)
      masterPassword.value = ''
    }
    toast.success('Relay settings saved')
  } catch (e) {
    toast.error(String(e))
  } finally {
    savingConfig.value = false
  }
}

async function toggleEnabled() {
  togglingEnabled.value = true
  try {
    if (enabled.value) {
      await store.disable()
      toast.success('Remote Control disabled')
    } else {
      await store.enable()
      toast.success('Remote Control enabled')
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    togglingEnabled.value = false
  }
}

async function clearMasterPassword() {
  if (savingMasterPw.value) return
  const ok = await confirm({
    title: 'Clear master password',
    message:
      'Remove the saved master password?\nYou will use the per-join OTP code again.',
    confirmLabel: 'Clear',
    variant: 'destructive',
  })
  if (!ok) return
  savingMasterPw.value = true
  try {
    await store.setMasterPassword('')
    masterPassword.value = ''
    toast.success('Master password cleared')
  } catch (e) {
    toast.error(String(e))
  } finally {
    savingMasterPw.value = false
  }
}

async function clearAudit() {
  if (clearingAudit.value) return
  const ok = await confirm({
    title: 'Clear audit log',
    message:
      'Permanently delete all remote-control audit entries?\nThis cannot be undone.',
    confirmLabel: 'Clear log',
    variant: 'destructive',
  })
  if (!ok) return
  clearingAudit.value = true
  try {
    const removed = await store.clearAudit()
    toast.success(removed > 0 ? `Cleared ${removed} audit entries` : 'Audit log is already empty')
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
        <h3 class="text-sm font-semibold flex-1">Remote Control</h3>
        <Badge v-if="enabled && connected" tone="success" class="gap-1">
          <Wifi class="h-3 w-3" :stroke-width="2" /> Connected
        </Badge>
        <Badge v-else-if="enabled && running" tone="warning" class="gap-1">
          <WifiOff class="h-3 w-3" :stroke-width="2" /> Waiting
        </Badge>
        <Badge v-else tone="neutral">Off</Badge>
      </template>
      <div class="p-4 space-y-4">
        <p class="text-xs text-muted-foreground">
          Control this host from a remote device through your self-hosted relay.
          The relay only ever sees end-to-end encrypted traffic.
        </p>

        <!-- Master on/off switch. This is the primary control for the feature,
             so it reads as a switch rather than competing with "Save settings". -->
        <div
          class="flex items-center justify-between gap-4 rounded-lg border border-border/60 bg-muted/20 px-3.5 py-3"
        >
          <div class="min-w-0">
            <p class="text-sm font-medium text-foreground">Enable Remote Control</p>
            <p class="mt-0.5 text-[11px] text-muted-foreground">
              <template v-if="enabled && connected">Connected to a controller device.</template>
              <template v-else-if="enabled">Running — waiting for a device to connect.</template>
              <template v-else-if="!canToggle">Save a Relay URL before enabling.</template>
              <template v-else>Off. Turn on to allow remote control.</template>
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

        <div class="space-y-1.5">
          <label class="text-xs font-medium text-muted-foreground">Relay URL</label>
          <Input
            v-model="relayUrl"
            placeholder="wss://relay.example.com/ws"
            :disabled="savingConfig"
          />
        </div>

        <div class="space-y-1.5">
          <label class="text-xs font-medium text-muted-foreground">
            Auth token
            <span v-if="status?.has_auth_token" class="text-emerald-500">(saved)</span>
          </label>
          <Input
            v-model="authToken"
            type="password"
            placeholder="Leave blank to keep the current token"
            :disabled="savingConfig"
          />
          <p class="text-[11px] text-muted-foreground/70">
            Stored in the OS keychain, never on disk in cleartext.
          </p>
        </div>

        <!-- Master password: an optional stable code that replaces the per-join
             OTP for a single owner (falls back to OTP when unset). -->
        <div class="space-y-1.5">
          <label class="flex items-center gap-2 text-xs font-medium text-muted-foreground">
            <span>
              Master password
              <span v-if="status?.has_master_password" class="text-emerald-500">(set)</span>
            </span>
            <button
              v-if="status?.has_master_password"
              type="button"
              :disabled="savingMasterPw"
              class="ml-auto font-normal text-[11px] text-muted-foreground/80 hover:text-destructive transition-colors disabled:opacity-50"
              @click="clearMasterPassword"
            >
              Clear
            </button>
          </label>
          <Input
            v-model="masterPassword"
            type="password"
            :placeholder="status?.has_master_password
              ? 'Enter a new password to replace the current one'
              : 'Set a password to skip the OTP each time'"
            :disabled="savingConfig || savingMasterPw"
            @keyup.enter="saveConfig"
          />
          <p class="text-[11px] text-muted-foreground/70">
            Once set, enter this password on your phone instead of the OTP code.
            Stored in the OS keychain. More convenient but fixed — only use it if
            you are the sole user. Saved together with the settings below.
          </p>
        </div>

        <div class="flex items-center gap-2 pt-1">
          <Button variant="outline" :disabled="savingConfig" @click="saveConfig">
            Save settings
          </Button>
          <span v-if="status?.device_id" class="ml-auto text-[11px] text-muted-foreground/60 font-mono">
            host: {{ status.device_id.slice(0, 8) }}
          </span>
        </div>

        <p class="text-[11px] text-muted-foreground/70 border-t border-border/50 pt-3">
          Remote sessions are now created per run: open a run and click the
          <span class="font-medium text-foreground/80">Remote</span> button in the
          toolbar to generate a link. Enter your
          <span class="font-medium text-foreground/80">master password</span>
          (if set) or the OTP code on your phone to connect.
        </p>
      </div>
    </Card>

    <!-- Audit log -->
    <Card>
      <template #header>
        <ShieldCheck class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1">Audit log</h3>
        <div class="flex items-center gap-1">
          <button
            class="p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent transition-colors"
            title="Refresh"
            @click="store.refreshAudit().catch(() => {})"
          >
            <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
          <button
            class="p-1 rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            title="Clear log"
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
              <th class="px-3 py-2 text-left font-medium">Time</th>
              <th class="px-3 py-2 text-left font-medium">Action</th>
              <th class="px-3 py-2 text-left font-medium">Result</th>
              <th class="px-3 py-2 text-left font-medium">Run</th>
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
        No remote activity recorded yet.
      </div>
    </Card>
  </div>
</template>
