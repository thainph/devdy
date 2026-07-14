import { invoke } from '@/lib/tauri'

export interface WorkDigestFilter {
  from?: string | null
  to?: string | null
  project_ids?: string[] | null
}

export interface WorkItem {
  id: string
  project_id: string
  project_name: string
  run_type: string
  ref_number: number | null
  status: string
  engine: string
  title: string | null
  description: string
  started_at: string | null
  finished_at: string | null
  created_at: string
  wall_secs: number
  active_secs: number | null
  tokens: number
  cost: number
}

export interface ProjectGroup {
  project_id: string | null
  project_name: string | null
  item_count: number
  wall_secs: number
  active_secs: number
  tokens: number
  cost: number
  items: WorkItem[]
}

export interface WorkDigestSummary {
  total_items: number
  total_wall_secs: number
  total_active_secs: number
  total_tokens: number
  total_cost: number
}

export interface WorkDigestResult {
  summary: WorkDigestSummary
  projects: ProjectGroup[]
}

export function getWorkDigest(filter: WorkDigestFilter): Promise<WorkDigestResult> {
  return invoke<WorkDigestResult>('get_work_digest', { filter })
}
