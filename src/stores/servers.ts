import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export type AuthMethod = 'agent' | 'key'

/** Server summary — mirrors the Rust `VpsServer` (never the passphrase VALUE). */
export interface VpsServer {
  id: string
  label: string
  host: string
  port: number
  username: string
  auth_method: AuthMethod
  private_key_path: string | null
  tags: string | null
  status: 'online' | 'offline' | 'unknown' | null
  last_checked_at: string | null
  has_passphrase: boolean
  created_at: string
}

export interface CreateServerPayload {
  label: string
  host: string
  port?: number | null
  username: string
  auth_method: AuthMethod
  private_key_path?: string | null
  private_key_source_path?: string | null
  tags?: string | null
  // null/'' → not stored (create) / kept unchanged (update).
  passphrase?: string | null
}

export interface UpdateServerPayload extends CreateServerPayload {
  id: string
}

export interface TestConnectionResult {
  ok: boolean
  message: string
}

/** A server mapped to a project under a deployment role — mirrors the Rust
 * `ProjectServer` (server summary + `role`, never the passphrase VALUE). */
export interface ProjectServer extends VpsServer {
  role: string
}

export const useServersStore = defineStore('servers', () => {
  const items = ref<VpsServer[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetchServers() {
    loading.value = true
    error.value = null
    try {
      items.value = await invoke<VpsServer[]>('list_vps_servers')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function createServer(payload: CreateServerPayload): Promise<VpsServer> {
    const server = await invoke<VpsServer>('create_vps_server', { payload })
    await fetchServers()
    return server
  }

  async function updateServer(payload: UpdateServerPayload): Promise<VpsServer> {
    const server = await invoke<VpsServer>('update_vps_server', { payload })
    await fetchServers()
    return server
  }

  async function deleteServer(id: string): Promise<void> {
    await invoke('delete_vps_server', { id })
    await fetchServers()
  }

  async function testConnection(id: string): Promise<TestConnectionResult> {
    const result = await invoke<TestConnectionResult>('test_vps_connection', { id })
    // Reflect the freshly persisted status/last_checked_at in the list.
    await fetchServers()
    return result
  }

  // --- Per-project mapping (GĐ2) ---

  /** List the servers mapped to a project (server summary + role). */
  async function listForProject(projectId: string): Promise<ProjectServer[]> {
    return invoke<ProjectServer[]>('list_project_servers', { projectId })
  }

  /** Map a server to a project under a role (empty role → 'production'). */
  async function mapToProject(
    projectId: string,
    serverId: string,
    role: string,
  ): Promise<void> {
    await invoke('map_server_to_project', { projectId, serverId, role })
  }

  /** Remove exactly the (project, server, role) mapping. */
  async function unmap(projectId: string, serverId: string, role: string): Promise<void> {
    await invoke('unmap_server', { projectId, serverId, role })
  }

  return {
    items,
    loading,
    error,
    fetchServers,
    createServer,
    updateServer,
    deleteServer,
    testConnection,
    listForProject,
    mapToProject,
    unmap,
  }
})
