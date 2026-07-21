<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  useServersStore,
  type AuthMethod,
  type TestConnectionResult,
  type VpsServer,
} from '@/stores/servers'
import { open } from '@tauri-apps/plugin-dialog'
import { Button, Input, Card, AppSelect } from '@/components/ui'
import {
  ArrowLeft, HardDrive, Save, Plug, CheckCircle2, XCircle, AlertCircle, KeyRound, Upload, X,
} from 'lucide-vue-next'

const route = useRoute()
const router = useRouter()
const store = useServersStore()

const isNew = computed(() => route.name === 'server-new')
const serverId = computed(() => route.params.id as string | undefined)

const label = ref('')
const host = ref('')
const port = ref('22')
const username = ref('')
const authMethod = ref<AuthMethod>('agent')
const privateKeyPath = ref('')
const privateKeySourcePath = ref('')
const tags = ref('')
const passphrase = ref('')
// True once loaded for an existing server that already has a stored passphrase;
// leaving the field blank then keeps it (BR-005 / AC-008).
const hasStoredPassphrase = ref(false)

const loading = ref(false)
const saving = ref(false)
const testing = ref(false)
const testResult = ref<TestConnectionResult | null>(null)
const validationError = ref<string | null>(null)

const isKey = computed(() => authMethod.value === 'key')

const selectedPrivateKeyName = computed(() => basename(privateKeySourcePath.value))
const storedPrivateKeyName = computed(() => basename(privateKeyPath.value))
const privateKeyName = computed(() => selectedPrivateKeyName.value || storedPrivateKeyName.value)
const privateKeyStateLabel = computed(() => {
  if (selectedPrivateKeyName.value) return 'Selected key file'
  if (storedPrivateKeyName.value) return 'Stored key file'
  return 'No private key selected'
})

const authOptions = [
  { value: 'agent', label: 'agent (ssh-agent)' },
  { value: 'key', label: 'key (private key file)' },
]

function applyServer(s: VpsServer) {
  label.value = s.label
  host.value = s.host
  port.value = String(s.port)
  username.value = s.username
  authMethod.value = s.auth_method
  privateKeyPath.value = s.private_key_path ?? ''
  privateKeySourcePath.value = ''
  tags.value = s.tags ?? ''
  hasStoredPassphrase.value = s.has_passphrase
}

function basename(path: string): string {
  const clean = path.trim().replace(/[/\\]+$/, '')
  if (!clean) return ''
  const slash = Math.max(clean.lastIndexOf('/'), clean.lastIndexOf('\\'))
  return slash >= 0 ? clean.slice(slash + 1) : clean
}

async function handleChoosePrivateKey() {
  const selected = await open({
    multiple: false,
    title: 'Select SSH Private Key',
  })
  const path = Array.isArray(selected) ? selected[0] : selected
  if (!path) return
  privateKeySourcePath.value = path
  if (validationError.value === "Auth method 'key' requires a private key file") {
    validationError.value = null
  }
}

function clearSelectedPrivateKey() {
  privateKeySourcePath.value = ''
}

onMounted(async () => {
  if (!isNew.value && serverId.value) {
    loading.value = true
    try {
      await store.fetchServers()
      const found = store.items.find(s => s.id === serverId.value)
      if (!found) throw new Error('Server not found')
      applyServer(found)
    } catch (e) {
      alert(String(e))
      router.push('/servers')
    } finally {
      loading.value = false
    }
  }
})

function validate(): string | null {
  if (!label.value.trim()) return 'Label is required'
  if (!host.value.trim()) return 'Host is required'
  if (!username.value.trim()) return 'Username is required'
  const p = Number(port.value)
  if (!Number.isInteger(p) || p < 1 || p > 65535) return 'Port must be between 1 and 65535'
  if (isKey.value && !privateKeyPath.value.trim() && !privateKeySourcePath.value.trim()) {
    return "Auth method 'key' requires a private key file"
  }
  return null
}

async function handleTest() {
  if (isNew.value || !serverId.value) {
    testResult.value = { ok: false, message: 'Save the server first, then test its connection.' }
    return
  }
  testing.value = true
  testResult.value = null
  try {
    testResult.value = await store.testConnection(serverId.value)
  } catch (e) {
    testResult.value = { ok: false, message: String(e) }
  } finally {
    testing.value = false
  }
}

async function handleSave() {
  const err = validate()
  if (err) {
    validationError.value = err
    return
  }
  validationError.value = null
  saving.value = true

  // Passphrase: omit when blank so an existing secret is kept untouched.
  const pass = passphrase.value.length > 0 ? passphrase.value : null
  const base = {
    label: label.value.trim(),
    host: host.value.trim(),
    port: Number(port.value),
    username: username.value.trim(),
    auth_method: authMethod.value,
    private_key_path: isKey.value ? privateKeyPath.value.trim() : null,
    private_key_source_path: isKey.value && privateKeySourcePath.value.trim()
      ? privateKeySourcePath.value.trim()
      : null,
    tags: tags.value.trim() ? tags.value.trim() : null,
    passphrase: pass,
  }
  try {
    if (isNew.value) {
      await store.createServer(base)
    } else {
      await store.updateServer({ id: serverId.value!, ...base })
    }
    router.push('/servers')
  } catch (e) {
    alert(String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Toolbar -->
    <div class="flex items-center gap-2 px-4 h-13 border-b border-border/60 bg-card/40 shrink-0">
      <Button
        variant="ghost"
        size="icon-sm"
        title="Back to Servers"
        @click="router.push('/servers')"
      >
        <ArrowLeft class="h-4 w-4" :stroke-width="1.75" />
      </Button>

      <div class="w-px h-4 bg-border/60 mx-1" />

      <div class="flex items-center gap-1.5 text-sm">
        <HardDrive class="h-4 w-4 text-muted-foreground" :stroke-width="1.5" />
        <span class="font-medium">{{ isNew ? 'New Server' : (label || '…') }}</span>
        <span v-if="!isNew" class="text-muted-foreground font-normal">— editing</span>
      </div>

      <div class="flex-1" />

      <!-- Validation error inline -->
      <div
        v-if="validationError"
        class="flex items-center gap-1 px-2.5 py-1 text-xs text-amber-500 bg-amber-500/10 border border-amber-500/20 rounded-md"
      >
        <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span class="max-w-48 truncate">{{ validationError }}</span>
      </div>

      <Button :disabled="saving" title="Save" @click="handleSave">
        <Save class="h-3.5 w-3.5" :stroke-width="2" />
        {{ saving ? 'Saving…' : 'Save' }}
      </Button>
    </div>

    <!-- Loading -->
    <div v-if="loading" class="flex-1 flex items-center justify-center text-muted-foreground text-sm">
      Loading…
    </div>

    <!-- Form -->
    <div v-else class="flex-1 overflow-auto p-6">
      <div class="max-w-xl space-y-4">
        <!-- General -->
        <Card body-class="p-4 space-y-4">
          <template #header>
            <HardDrive class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">General</span>
          </template>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Label</label>
            <Input v-model="label" size="sm" placeholder="Production SG" />
          </div>
          <div class="flex gap-3">
            <div class="space-y-1.5 flex-1">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Host</label>
              <Input v-model="host" size="sm" placeholder="1.2.3.4 or vps.example.com" class="font-mono" />
            </div>
            <div class="space-y-1.5 w-24">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Port</label>
              <Input v-model="port" size="sm" type="number" placeholder="22" class="font-mono" />
            </div>
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Username</label>
            <Input v-model="username" size="sm" placeholder="root" class="font-mono" />
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Tags</label>
            <Input v-model="tags" size="sm" placeholder="prod, sg (comma-separated)" />
          </div>
        </Card>

        <!-- Authentication -->
        <Card body-class="p-4 space-y-4">
          <template #header>
            <KeyRound class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Authentication</span>
          </template>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Auth method</label>
            <AppSelect
              size="sm"
              :model-value="authMethod"
              :options="authOptions"
              @update:model-value="(v: string) => authMethod = v as AuthMethod"
            />
            <p v-if="!isKey" class="text-[10px] text-muted-foreground flex items-center gap-1">
              <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              Uses the running ssh-agent; no key path needed.
            </p>
          </div>
          <div v-if="isKey" class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Private key file</label>
            <div class="flex items-center gap-2">
              <div class="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md border border-border bg-muted/30 px-2.5 text-xs">
                <KeyRound class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <div class="min-w-0 flex-1">
                  <div class="truncate font-mono" :class="privateKeyName ? 'text-foreground' : 'text-muted-foreground'">
                    {{ privateKeyName || privateKeyStateLabel }}
                  </div>
                </div>
                <span
                  v-if="privateKeyName"
                  class="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground"
                >
                  {{ privateKeyStateLabel }}
                </span>
              </div>
              <Button variant="outline" size="sm" @click="handleChoosePrivateKey">
                <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
                {{ privateKeyName ? 'Change' : 'Choose' }}
              </Button>
              <Button
                v-if="privateKeySourcePath"
                variant="ghost"
                size="icon-sm"
                title="Clear selected key"
                @click="clearSelectedPrivateKey"
              >
                <X class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </div>
            <p class="text-[10px] text-muted-foreground">
              The selected key is copied into Devdy app data on save.
            </p>
          </div>
          <div v-if="isKey" class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Passphrase</label>
            <Input
              v-model="passphrase"
              size="sm"
              type="password"
              :placeholder="hasStoredPassphrase ? '•••••• (unchanged)' : 'optional'"
              class="font-mono"
            />
            <p class="text-[10px] text-muted-foreground">
              Stored in the OS keychain only. Leave blank to keep the existing value.
            </p>
          </div>
        </Card>

        <!-- Test connection -->
        <Card body-class="p-4 space-y-3">
          <template #header>
            <Plug class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Connection</span>
          </template>
          <div class="flex items-center gap-3">
            <Button variant="outline" :disabled="testing" @click="handleTest">
              <Plug class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ testing ? 'Testing…' : 'Test connection' }}
            </Button>
            <span v-if="isNew" class="text-[11px] text-muted-foreground">Save first to enable testing</span>
          </div>
          <div
            v-if="testResult"
            class="p-3 rounded-md text-xs border flex items-center gap-1.5"
            :class="testResult.ok
              ? 'bg-emerald-500/10 border-emerald-500/20 text-emerald-500'
              : 'bg-destructive/10 border-destructive/20 text-destructive'"
          >
            <CheckCircle2 v-if="testResult.ok" class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
            <XCircle v-else class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
            <span>{{ testResult.message }}</span>
          </div>
        </Card>
      </div>
    </div>
  </div>
</template>
