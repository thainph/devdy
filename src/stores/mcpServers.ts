import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export type McpTransport = 'stdio' | 'http' | 'sse'

/** Server summary — mirrors the Rust `McpServer` (no secret VALUEs, only keys). */
export interface McpServer {
  id: string
  name: string
  description: string
  transport: McpTransport
  command: string | null
  args: string[]
  url: string | null
  env_keys: string[]
  header_keys: string[]
  enabled: boolean
  created_at: string
}

/** Server + per-project enabled flag — mirrors the Rust `ProjectMcpServer`. */
export interface ProjectMcpServer extends McpServer {
  enabled_for_project: boolean
}

/** One env/header secret row sent to the backend (`SecretEntry`). */
export interface SecretEntry {
  key: string
  // `undefined`/omitted on update = keep stored VALUE; a string overwrites it.
  value?: string
}

export interface CreateMcpServerPayload {
  name: string
  description: string
  transport: McpTransport
  command?: string | null
  args: string[]
  url?: string | null
  env: SecretEntry[]
  headers: SecretEntry[]
  enabled: boolean
}

export interface UpdateMcpServerPayload extends CreateMcpServerPayload {
  id: string
}

export interface TestConnectionResult {
  ok: boolean
  message: string
}

export const useMcpServersStore = defineStore('mcpServers', () => {
  const items = ref<McpServer[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetchServers() {
    loading.value = true
    error.value = null
    try {
      items.value = await invoke<McpServer[]>('list_mcp_servers')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function getServer(id: string): Promise<McpServer> {
    return invoke<McpServer>('get_mcp_server', { id })
  }

  async function createServer(payload: CreateMcpServerPayload): Promise<McpServer> {
    const server = await invoke<McpServer>('create_mcp_server', { payload })
    await fetchServers()
    return server
  }

  async function updateServer(payload: UpdateMcpServerPayload): Promise<McpServer> {
    const server = await invoke<McpServer>('update_mcp_server', { payload })
    await fetchServers()
    return server
  }

  async function deleteServer(id: string): Promise<void> {
    await invoke('delete_mcp_server', { id })
    await fetchServers()
  }

  async function listForProject(projectId: string): Promise<ProjectMcpServer[]> {
    return invoke<ProjectMcpServer[]>('list_project_mcp_servers', { projectId })
  }

  async function setForProject(projectId: string, serverIds: string[]): Promise<void> {
    await invoke('set_project_mcp_servers', { projectId, serverIds })
  }

  async function testConnection(id: string): Promise<TestConnectionResult> {
    return invoke<TestConnectionResult>('test_mcp_connection', { id })
  }

  async function exportServer(id: string, path: string): Promise<void> {
    await invoke('export_mcp_server', { id, path })
  }

  async function importServer(path: string): Promise<McpServer> {
    const server = await invoke<McpServer>('import_mcp_server', { path })
    await fetchServers()
    return server
  }

  return {
    items,
    loading,
    error,
    fetchServers,
    getServer,
    createServer,
    updateServer,
    deleteServer,
    listForProject,
    setForProject,
    testConnection,
    exportServer,
    importServer,
  }
})
