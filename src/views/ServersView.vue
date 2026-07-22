<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useServersStore, type VpsServer } from '@/stores/servers'
import { Button, Card, Badge } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { Plus, Pencil, Trash2, HardDrive, CalendarDays, Plug, KeyRound, UserCog } from 'lucide-vue-next'

const router = useRouter()
const store = useServersStore()
const { confirm } = useConfirm()
const deletingId = ref<string | null>(null)
const testingId = ref<string | null>(null)

// Status dot colour: online → green, offline → red, unknown/null → gray.
const statusColor: Record<string, string> = {
  online: 'bg-emerald-500',
  offline: 'bg-destructive',
}

function dotColor(status: VpsServer['status']): string {
  return (status && statusColor[status]) || 'bg-muted-foreground/40'
}

function statusLabel(status: VpsServer['status']): string {
  if (status === 'online') return 'Online'
  if (status === 'offline') return 'Offline'
  return 'Not checked'
}

function tagList(tags: string | null): string[] {
  return (tags ?? '')
    .split(',')
    .map(t => t.trim())
    .filter(t => t.length > 0)
}

onMounted(() => {
  store.fetchServers()
})

async function handleDelete(server: VpsServer) {
  if (!(await confirm({
    title: 'Delete server',
    message: `Delete server "${server.label}"? This also removes its stored passphrase. This cannot be undone.`,
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

async function handleTest(server: VpsServer) {
  testingId.value = server.id
  try {
    await store.testConnection(server.id)
  } catch (e) {
    alert(String(e))
  } finally {
    testingId.value = null
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
        <h1 class="text-sm font-semibold">Servers</h1>
        <span
          v-if="!store.loading && store.items.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.items.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button @click="router.push('/servers/new')">
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
          <HardDrive class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">No servers yet</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-50">Add a VPS once, then test its connection and reuse it across projects</p>
        <Button class="mt-4" @click="router.push('/servers/new')">
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
          @click="router.push(`/servers/${server.id}/edit`)"
        >
          <!-- hover accent line -->
          <span class="absolute inset-x-0 top-0 h-0.5 origin-left scale-x-0 bg-linear-to-r from-primary to-primary/30 transition-transform duration-200 group-hover:scale-x-100" />

          <!-- Icon + label/host -->
          <div class="flex items-start gap-3 mb-3">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary transition-colors group-hover:bg-primary/15 group-hover:border-primary/25">
              <HardDrive class="h-4.5 w-4.5" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5 flex-wrap">
                <p class="text-sm font-semibold truncate leading-tight">{{ server.label }}</p>
              </div>
              <p class="text-xs text-muted-foreground mt-1 font-mono truncate leading-relaxed">
                {{ server.username }}@{{ server.host }}:{{ server.port }}
              </p>
            </div>
          </div>

          <!-- Status + tags -->
          <div class="flex items-center gap-2 flex-wrap mb-3">
            <span class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
              <span class="h-2 w-2 rounded-full shrink-0" :class="dotColor(server.status)" />
              {{ statusLabel(server.status) }}
            </span>
            <Badge
              v-for="tag in tagList(server.tags)"
              :key="tag"
              tone="neutral"
              size="xs"
              class="shrink-0"
            >
              {{ tag }}
            </Badge>
          </div>

          <!-- Footer -->
          <div class="flex items-center justify-between pt-2.5 border-t border-border/60 mt-auto">
            <div class="flex items-center gap-2 min-w-0">
              <Badge tone="primary" size="xs" class="shrink-0 uppercase tracking-wide">
                <KeyRound v-if="server.auth_method === 'key'" class="h-2.5 w-2.5" :stroke-width="2" />
                <UserCog v-else class="h-2.5 w-2.5" :stroke-width="2" />
                {{ server.auth_method }}
              </Badge>
              <div class="flex items-center gap-1 text-[10px] text-muted-foreground/60">
                <CalendarDays class="h-3 w-3" :stroke-width="1.5" />
                <span>{{ formatDate(server.created_at) }}</span>
              </div>
            </div>
            <!-- Actions on hover -->
            <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity" @click.stop>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-40"
                title="Test connection"
                :disabled="testingId === server.id"
                @click="handleTest(server)"
              >
                <Plug class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                title="Edit"
                @click="router.push(`/servers/${server.id}/edit`)"
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
