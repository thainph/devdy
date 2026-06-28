import { invoke } from '@/lib/tauri'

export interface StatsFilter {
  from?: string | null
  to?: string | null
  engine?: string | null
  project_id?: string | null
}

export interface UsageSummary {
  total_tokens: number
  total_input: number
  total_output: number
  total_cache: number
  total_cost: number
  estimated_cost: number
  total_runs: number
  total_turns: number
}

export interface DailyPoint {
  date: string
  tokens: number
  cost: number
  runs: number
}

export interface EngineStat {
  engine: string
  tokens: number
  cost: number
  runs: number
}

export interface ProjectStat {
  project_id: string | null
  project_name: string | null
  tokens: number
  cost: number
  runs: number
}

export interface ModelStat {
  model: string | null
  tokens: number
  cost: number
  runs: number
}

export interface StatsResult {
  summary: UsageSummary
  daily: DailyPoint[]
  by_engine: EngineStat[]
  by_project: ProjectStat[]
  by_model: ModelStat[]
}

export interface BackfillResult {
  inserted: number
  runs_scanned: number
}

export interface ResetResult {
  runs_deleted: number
  usage_cleared: number
}

export interface StorageCategory {
  id: string
  label: string
  description: string
  path: string
  size_bytes: number
  file_count: number
  deletable: boolean
  destructive: boolean
}

export interface StorageStats {
  categories: StorageCategory[]
  total_bytes: number
  total_files: number
}

export interface CleanResult {
  category: string
  deleted_files: number
  freed_bytes: number
}

export function getStorageStats(): Promise<StorageStats> {
  return invoke<StorageStats>('get_storage_stats')
}

export function cleanStorage(category: string): Promise<CleanResult> {
  return invoke<CleanResult>('clean_storage', { category })
}

export function getUsageStats(filter: StatsFilter): Promise<StatsResult> {
  return invoke<StatsResult>('get_usage_stats', { filter })
}

export function backfillUsage(): Promise<BackfillResult> {
  return invoke<BackfillResult>('backfill_usage')
}

export function resetUsageStats(): Promise<ResetResult> {
  return invoke<ResetResult>('reset_usage_stats')
}
