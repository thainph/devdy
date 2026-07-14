<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useMcpServersStore, type McpServer } from '@/stores/mcpServers'
import { useAppSettingsStore } from '@/stores/appSettings'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Button, Card, Badge } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { Plus, Upload, Download, Pencil, Trash2, Server, CalendarDays, Power, AlertTriangle } from 'lucide-vue-next'

const router = useRouter()
const store = useMcpServersStore()
const appSettings = useAppSettingsStore()
const { confirm } = useConfirm()
const deletingId = ref<string | null>(null)
const togglingId = ref<string | null>(null)
const importing = ref(false)

// The MCP list is global (not project-scoped). Per AC-14, flag remote servers
// as "Claude-only" only when the app's default engine is Codex, since Codex
// can't use http/sse transports.
const defaultIsCodex = computed(() => appSettings.settings?.default_engine === 'codex')

const transportTone: Record<string, 'primary' | 'info' | 'neutral'> = {
  stdio: 'primary',
  http: 'info',
  sse: 'info',
}

onMounted(() => {
  store.fetchServers()
  appSettings.ensureLoaded()
})

function isRemote(server: McpServer): boolean {
  return server.transport === 'http' || server.transport === 'sse'
}

async function handleImport() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'MCP Server', extensions: ['json'] }],
  })
  if (!selected) return
  importing.value = true
  try {
    await store.importServer(selected as string)
  } catch (e) {
    alert(String(e))
  } finally {
    importing.value = false
  }
}

async function handleExport(server: McpServer) {
  const destPath = await save({
    defaultPath: `${server.name}.json`,
    filters: [{ name: 'JSON', extensions: ['json'] }],
  })
  if (!destPath) return
  try {
    await store.exportServer(server.id, destPath)
  } catch (e) {
    alert(String(e))
  }
}

async function handleDelete(server: McpServer) {
  if (!(await confirm({
    title: 'Delete MCP server',
    message: `Delete MCP server "${server.name}"? This also removes its stored secrets. This cannot be undone.`,
    confirmLabel: 'Delete',
  }))) return
  deletingId.value = server.id
  try {
    await store.deleteServer(server.id)
  } catch (e) {
    alert(String(e))
  } finally {
    deletingId.value = null
  }
}

// Toggling `enabled` re-uses update. Secret rows are sent as key-only entries
// (value omitted) so the backend keeps the stored VALUEs untouched.
async function handleToggleEnabled(server: McpServer) {
  togglingId.value = server.id
  try {
    await store.updateServer({
      id: server.id,
      name: server.name,
      description: server.description,
      transport: server.transport,
      command: server.command,
      args: server.args,
      url: server.url,
      env: server.env_keys.map(key => ({ key })),
      headers: server.header_keys.map(key => ({ key })),
      enabled: !server.enabled,
    })
  } catch (e) {
    alert(String(e))
  } finally {
    togglingId.value = null
  }
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">MCP Servers</h1>
        <span
          v-if="!store.loading && store.items.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.items.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button
          variant="outline"
          :disabled="importing"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ importing ? 'Importing…' : 'Import' }}
        </Button>
        <Button @click="router.push('/mcp/new')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New Server
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Loading skeleton -->
      <div v-if="store.loading" class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <div v-for="i in 5" :key="i" class="h-27 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="store.items.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <Server class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">No MCP servers yet</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-50">Define an MCP server once, then enable it per project</p>
        <Button class="mt-4" @click="router.push('/mcp/new')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New Server
        </Button>
      </div>

      <!-- Cards grid -->
      <div v-else class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <Card
          v-for="server in store.items"
          :key="server.id"
          class="group relative flex flex-col transition-all duration-150 cursor-pointer hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)] hover:-translate-y-0.5"
          body-class="flex flex-col flex-1 p-4"
          :class="!server.enabled && 'opacity-60'"
          @click="router.push(`/mcp/${server.id}/edit`)"
        >
          <!-- hover accent line -->
          <span class="absolute inset-x-0 top-0 h-0.5 origin-left scale-x-0 bg-linear-to-r from-primary to-primary/30 transition-transform duration-200 group-hover:scale-x-100" />

          <!-- Icon + name/desc -->
          <div class="flex items-start gap-3 mb-3">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary transition-colors group-hover:bg-primary/15 group-hover:border-primary/25">
              <Server class="h-4.5 w-4.5" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5 flex-wrap">
                <p class="text-sm font-semibold font-mono truncate leading-tight">{{ server.name }}</p>
                <Badge :tone="transportTone[server.transport]" size="xs" class="shrink-0 uppercase tracking-wide">
                  {{ server.transport }}
                </Badge>
                <Badge
                  v-if="isRemote(server) && defaultIsCodex"
                  tone="warning"
                  size="xs"
                  class="shrink-0"
                  title="Remote transports (http/sse) are only supported by Claude. The default engine is Codex, which will skip this server."
                >
                  <AlertTriangle class="h-2.5 w-2.5" :stroke-width="2" />
                  Claude only
                </Badge>
              </div>
              <p class="text-xs text-muted-foreground mt-1 line-clamp-2 leading-relaxed">{{ server.description }}</p>
            </div>
          </div>

          <!-- Footer -->
          <div class="flex items-center justify-between pt-2.5 border-t border-border/60 mt-auto">
            <div class="flex items-center gap-1 text-[10px] text-muted-foreground/60">
              <CalendarDays class="h-3 w-3" :stroke-width="1.5" />
              <span>{{ formatDate(server.created_at) }}</span>
            </div>
            <!-- Actions on hover -->
            <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity" @click.stop>
              <button
                class="flex h-6 w-6 items-center justify-center rounded transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-default"
                :class="server.enabled ? 'text-emerald-500 hover:bg-accent' : 'text-muted-foreground hover:text-foreground hover:bg-accent'"
                :title="server.enabled ? 'Disable' : 'Enable'"
                :disabled="togglingId === server.id"
                @click="handleToggleEnabled(server)"
              >
                <Power class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Export JSON (includes secrets)"
                @click="handleExport(server)"
              >
                <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Edit"
                @click="router.push(`/mcp/${server.id}/edit`)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-destructive/60 hover:text-destructive hover:bg-destructive/10 transition-colors cursor-pointer disabled:opacity-40"
                title="Delete"
                :disabled="deletingId === server.id"
                @click="handleDelete(server)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  </div>
</template>
