<script setup lang="ts">
// Host-side Remote Control settings. Lets the Owner enable the feature,
// configure the relay, and review the audit log. Remote sessions are now
// created per-run via the Remote button inside a run (RemoteSessionModal) —
// there is no global device pairing anymore.
import { onMounted, onUnmounted, ref, computed } from 'vue'
import { Button, Input, Card, Badge } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { Radio, Wifi, WifiOff, ShieldCheck, RefreshCw } from 'lucide-vue-next'

const store = useRemoteControlStore()
const { toast } = useToast()

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

async function saveMasterPassword() {
  savingMasterPw.value = true
  try {
    const had = masterPassword.value.trim().length > 0
    await store.setMasterPassword(masterPassword.value)
    masterPassword.value = ''
    toast.success(had ? 'Đã lưu master password' : 'Đã xoá master password')
  } catch (e) {
    toast.error(String(e))
  } finally {
    savingMasterPw.value = false
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

        <div class="flex items-center gap-2 pt-1">
          <Button variant="outline" :disabled="savingConfig" @click="saveConfig">
            Save settings
          </Button>
          <Button
            :variant="enabled ? 'destructive' : 'primary'"
            :disabled="togglingEnabled"
            @click="toggleEnabled"
          >
            {{ enabled ? 'Disable' : 'Enable' }}
          </Button>
          <span v-if="status?.device_id" class="ml-auto text-[11px] text-muted-foreground/60 font-mono">
            host: {{ status.device_id.slice(0, 8) }}
          </span>
        </div>

        <!-- Master password: an optional stable code that replaces the per-join
             OTP for a single owner (falls back to OTP when unset). -->
        <div class="space-y-1.5 border-t border-border/50 pt-4">
          <label class="text-xs font-medium text-muted-foreground">
            Master password
            <span v-if="status?.has_master_password" class="text-emerald-500">(đã đặt)</span>
          </label>
          <div class="flex items-center gap-2">
            <Input
              v-model="masterPassword"
              type="password"
              :placeholder="status?.has_master_password
                ? 'Nhập mật khẩu mới, hoặc để trống để xoá'
                : 'Đặt mật khẩu để bỏ nhập OTP mỗi lần'"
              :disabled="savingMasterPw"
              class="flex-1"
              @keyup.enter="saveMasterPassword"
            />
            <Button variant="outline" :disabled="savingMasterPw" @click="saveMasterPassword">
              {{ status?.has_master_password && !masterPassword ? 'Xoá' : 'Lưu' }}
            </Button>
          </div>
          <p class="text-[11px] text-muted-foreground/70">
            Khi đã đặt, trên điện thoại bạn nhập mật khẩu này thay cho mã OTP.
            Lưu trong OS keychain. Tiện hơn nhưng cố định — chỉ nên dùng khi bạn
            là người dùng duy nhất.
          </p>
        </div>

        <p class="text-[11px] text-muted-foreground/70 border-t border-border/50 pt-3">
          Phiên remote giờ được tạo theo từng run: mở một run rồi bấm nút
          <span class="font-medium text-foreground/80">Remote</span> trên thanh
          công cụ để tạo link. Nhập <span class="font-medium text-foreground/80">master
          password</span> (nếu đã đặt) hoặc mã OTP trên điện thoại để kết nối.
        </p>
      </div>
    </Card>

    <!-- Audit log -->
    <Card>
      <template #header>
        <ShieldCheck class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1">Audit log</h3>
        <button
          class="text-muted-foreground hover:text-foreground transition-colors"
          title="Refresh"
          @click="store.refreshAudit().catch(() => {})"
        >
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
        </button>
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
