import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'

/** Deploy playbook for a (project, server, role) triple — mirrors the Rust
 * `DeployPlaybook`. Never carries a passphrase/secret (SEC-201). */
export interface DeployPlaybook {
  id: string
  project_id: string
  server_id: string
  role: string
  remote_path: string | null
  branch: string | null
  instructions: string | null
  created_at: string
  updated_at: string
}

/** Payload for `save_deploy_playbook` — mirrors the Rust
 * `SaveDeployPlaybookPayload` (empty role → 'production' server-side). */
export interface SaveDeployPlaybookPayload {
  project_id: string
  server_id: string
  role: string
  remote_path?: string | null
  branch?: string | null
  instructions?: string | null
}

/** Payload for `start_deploy` — mirrors the Rust `StartDeployPayload`. */
export interface StartDeployPayload {
  project_id: string
  server_id: string
  role: string
  confirm_production?: boolean
  engine_override?: string | null
  model_override?: string | null
}

/** Result of `start_deploy` — mirrors the Rust `StartDeployResult`. The
 * frontend feeds `prompt` into the existing `start_run` as `prompt_override`. */
export interface StartDeployResult {
  run_id: string
  prompt: string
}

/** One deploy run in the history list — mirrors the Rust `DeployHistoryItem`. */
export interface DeployHistoryItem {
  run_id: string
  server_id: string
  server_label: string
  role: string | null
  status: string
  engine: string
  title: string | null
  created_at: string
}

export const useDeploymentsStore = defineStore('deployments', () => {
  /** AC-202: fetch the playbook for (project, server, role) or null. */
  async function getPlaybook(
    projectId: string,
    serverId: string,
    role: string,
  ): Promise<DeployPlaybook | null> {
    return invoke<DeployPlaybook | null>('get_deploy_playbook', {
      projectId,
      serverId,
      role,
    })
  }

  /** AC-201 / BR-201: upsert the playbook by (project, server, role). */
  async function savePlaybook(payload: SaveDeployPlaybookPayload): Promise<DeployPlaybook> {
    return invoke<DeployPlaybook>('save_deploy_playbook', { payload })
  }

  /** AC-203..207: create the deploy run (does NOT spawn) and return
   * { run_id, prompt }. The caller then drives `start_run`. */
  async function startDeploy(payload: StartDeployPayload): Promise<StartDeployResult> {
    return invoke<StartDeployResult>('start_deploy', { payload })
  }

  /** AC-208: list this project's deploy runs, newest first. */
  async function listHistory(projectId: string): Promise<DeployHistoryItem[]> {
    return invoke<DeployHistoryItem[]>('list_deploy_history', { projectId })
  }

  return {
    getPlaybook,
    savePlaybook,
    startDeploy,
    listHistory,
  }
})
