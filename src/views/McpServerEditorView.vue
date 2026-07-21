<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  useMcpServersStore,
  type McpTransport,
  type SecretEntry,
  type TestConnectionResult,
} from '@/stores/mcpServers'
import { Button, Input, Textarea, Card, Badge, AppSelect } from '@/components/ui'
import {
  ArrowLeft, Server, Save, Plus, Trash2, Plug, CheckCircle2, XCircle, AlertCircle,
} from 'lucide-vue-next'

const route = useRoute()
const router = useRouter()
const store = useMcpServersStore()

const isNew = computed(() => route.name === 'mcp-new')
const serverId = computed(() => route.params.id as string | undefined)

// One editable secret row. `existing` marks a key that already has a stored
// VALUE (never surfaced to the UI); an empty `value` on such a row = keep it.
interface SecretRow {
  key: string
  value: string
  existing: boolean
}

const name = ref('')
const description = ref('')
const transport = ref<McpTransport>('stdio')
const command = ref('')
const argsText = ref('')
const url = ref('')
const envRows = ref<SecretRow[]>([])
const headerRows = ref<SecretRow[]>([])
const enabled = ref(true)

const loading = ref(false)
const saving = ref(false)
const testing = ref(false)
const testResult = ref<TestConnectionResult | null>(null)
const validationError = ref<string | null>(null)

const isRemote = computed(() => transport.value === 'http' || transport.value === 'sse')
const isSse = computed(() => transport.value === 'sse')

const transportOptions = [
  { value: 'stdio', label: 'stdio (local process)' },
  { value: 'http', label: 'http (remote)' },
  { value: 'sse', label: 'sse (remote)' },
]

onMounted(async () => {
  if (!isNew.value && serverId.value) {
    loading.value = true
    try {
      const s = await store.getServer(serverId.value)
      name.value = s.name
      description.value = s.description
      transport.value = s.transport
      command.value = s.command ?? ''
      argsText.value = (s.args ?? []).join('\n')
      url.value = s.url ?? ''
      // Existing keys come back without VALUEs — mark them so we don't resend.
      envRows.value = s.env_keys.map(key => ({ key, value: '', existing: true }))
      headerRows.value = s.header_keys.map(key => ({ key, value: '', existing: true }))
      enabled.value = s.enabled
    } catch (e) {
      alert(String(e))
      router.push('/mcp')
    } finally {
      loading.value = false
    }
  }
})

function addEnvRow() {
  envRows.value.push({ key: '', value: '', existing: false })
}
function removeEnvRow(i: number) {
  envRows.value.splice(i, 1)
}
function addHeaderRow() {
  headerRows.value.push({ key: '', value: '', existing: false })
}
function removeHeaderRow(i: number) {
  headerRows.value.splice(i, 1)
}

function parsedArgs(): string[] {
  return argsText.value
    .split('\n')
    .map(a => a.trim())
    .filter(a => a.length > 0)
}

// Build SecretEntry rows for the backend. For an existing key left blank we
// OMIT `value` (keep stored). A touched/new key sends its VALUE.
function toSecretEntries(rows: SecretRow[]): SecretEntry[] {
  return rows
    .filter(r => r.key.trim().length > 0)
    .map(r => {
      if (r.existing && r.value === '') return { key: r.key.trim() }
      return { key: r.key.trim(), value: r.value }
    })
}

function validate(): string | null {
  if (!name.value.trim()) return 'Name is required'
  if (!/^[a-zA-Z0-9_-]+$/.test(name.value.trim())) {
    return 'Name may only contain letters, numbers, hyphens, and underscores'
  }
  if (transport.value === 'stdio') {
    if (!command.value.trim()) return 'stdio transport requires a command'
  } else if (!url.value.trim()) {
    return `${transport.value} transport requires a url`
  }
  const rows = isRemote.value ? headerRows.value : envRows.value
  const seen = new Set<string>()
  for (const r of rows) {
    const k = r.key.trim()
    if (!k) return 'Secret key cannot be empty'
    if (seen.has(k)) return `Duplicate key "${k}"`
    seen.add(k)
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
  // Send only the transport-relevant secret set; the backend nulls out the
  // irrelevant fields for the chosen transport anyway.
  const env = transport.value === 'stdio' ? toSecretEntries(envRows.value) : []
  const headers = isRemote.value ? toSecretEntries(headerRows.value) : []
  const base = {
    name: name.value.trim(),
    description: description.value,
    transport: transport.value,
    command: transport.value === 'stdio' ? command.value.trim() : null,
    args: transport.value === 'stdio' ? parsedArgs() : [],
    url: isRemote.value ? url.value.trim() : null,
    env,
    headers,
    enabled: enabled.value,
  }
  try {
    if (isNew.value) {
      await store.createServer(base)
    } else {
      await store.updateServer({ id: serverId.value!, ...base })
    }
    router.push('/mcp')
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
        title="Back to MCP Servers"
        @click="router.push('/mcp')"
      >
        <ArrowLeft class="h-4 w-4" :stroke-width="1.75" />
      </Button>

      <div class="w-px h-4 bg-border/60 mx-1" />

      <div class="flex items-center gap-1.5 text-sm">
        <Server class="h-4 w-4 text-muted-foreground" :stroke-width="1.5" />
        <span class="font-medium">{{ isNew ? 'New MCP Server' : (name || '…') }}</span>
        <span v-if="!isNew" class="text-muted-foreground font-normal">— editing</span>
      </div>

      <div class="flex-1" />

      <!-- Enabled toggle -->
      <button
        type="button"
        class="flex items-center gap-1.5 text-xs cursor-pointer select-none px-2 py-1 rounded-md hover:bg-accent transition-colors"
        @click="enabled = !enabled"
      >
        <span
          class="relative inline-flex h-4 w-7 items-center rounded-full transition-colors"
          :class="enabled ? 'bg-primary' : 'bg-muted'"
        >
          <span
            class="inline-block h-3 w-3 transform rounded-full bg-white transition-transform"
            :class="enabled ? 'translate-x-3.5' : 'translate-x-0.5'"
          />
        </span>
        <span class="text-muted-foreground">{{ enabled ? 'Enabled' : 'Disabled' }}</span>
      </button>

      <!-- Validation error inline -->
      <div
        v-if="validationError"
        class="flex items-center gap-1 px-2.5 py-1 text-xs text-amber-500 bg-amber-500/10 border border-amber-500/20 rounded-md"
      >
        <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span class="max-w-48 truncate">{{ validationError }}</span>
      </div>

      <Button :disabled="saving" @click="handleSave" title="Save">
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
            <Server class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">General</span>
          </template>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Name</label>
            <Input v-model="name" size="sm" placeholder="my-mcp-server" class="font-mono" />
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Description</label>
            <Input v-model="description" size="sm" placeholder="What this server provides" />
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Transport</label>
            <AppSelect
              size="sm"
              :model-value="transport"
              :options="transportOptions"
              @update:model-value="(v: string) => transport = v as McpTransport"
            />
            <p v-if="isSse" class="text-[10px] text-amber-500 flex items-center gap-1">
              <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              SSE is Claude-only; Codex runs support stdio and streamable HTTP.
            </p>
          </div>
        </Card>

        <!-- stdio config -->
        <Card v-if="transport === 'stdio'" body-class="p-4 space-y-4">
          <template #header>
            <Plug class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Process</span>
          </template>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Command</label>
            <Input v-model="command" size="sm" placeholder="npx" class="font-mono" />
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Args (one per line)</label>
            <Textarea
              v-model="argsText"
              rows="3"
              placeholder="-y&#10;@modelcontextprotocol/server-filesystem"
              class="font-mono"
            />
          </div>

          <!-- Env key/value rows -->
          <div class="space-y-2">
            <div class="flex items-center justify-between">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Environment</label>
              <Button variant="outline" size="xs" @click="addEnvRow">
                <Plus class="h-3 w-3" :stroke-width="2" />
                Add
              </Button>
            </div>
            <div v-if="envRows.length === 0" class="text-[11px] text-muted-foreground">No environment variables</div>
            <div v-for="(row, i) in envRows" :key="i" class="flex items-center gap-1.5">
              <Input v-model="row.key" size="sm" placeholder="KEY" class="flex-1 font-mono" />
              <div class="flex-1 relative">
                <Input
                  v-model="row.value"
                  size="sm"
                  type="password"
                  :placeholder="row.existing ? '•••••• (unchanged)' : 'value'"
                  class="font-mono"
                />
              </div>
              <Badge v-if="row.existing" tone="neutral" size="xs" class="shrink-0" title="A value is stored; leave blank to keep it">stored</Badge>
              <Button variant="destructive-ghost" size="icon-sm" class="shrink-0" @click="removeEnvRow(i)">
                <Trash2 class="h-3 w-3" :stroke-width="1.75" />
              </Button>
            </div>
          </div>
        </Card>

        <!-- http/sse config -->
        <Card v-else body-class="p-4 space-y-4">
          <template #header>
            <Plug class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Endpoint</span>
          </template>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">URL</label>
            <Input v-model="url" size="sm" placeholder="https://example.com/mcp" class="font-mono" />
          </div>

          <!-- Header key/value rows -->
          <div class="space-y-2">
            <div class="flex items-center justify-between">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Headers</label>
              <Button variant="outline" size="xs" @click="addHeaderRow">
                <Plus class="h-3 w-3" :stroke-width="2" />
                Add
              </Button>
            </div>
            <div v-if="headerRows.length === 0" class="text-[11px] text-muted-foreground">No headers</div>
            <div v-for="(row, i) in headerRows" :key="i" class="flex items-center gap-1.5">
              <Input v-model="row.key" size="sm" placeholder="Authorization" class="flex-1 font-mono" />
              <div class="flex-1 relative">
                <Input
                  v-model="row.value"
                  size="sm"
                  type="password"
                  :placeholder="row.existing ? '•••••• (unchanged)' : 'value'"
                  class="font-mono"
                />
              </div>
              <Badge v-if="row.existing" tone="neutral" size="xs" class="shrink-0" title="A value is stored; leave blank to keep it">stored</Badge>
              <Button variant="destructive-ghost" size="icon-sm" class="shrink-0" @click="removeHeaderRow(i)">
                <Trash2 class="h-3 w-3" :stroke-width="1.75" />
              </Button>
            </div>
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
