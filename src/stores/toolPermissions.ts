import { defineStore } from 'pinia'
import { reactive } from 'vue'

/**
 * Standing tool permissions the user chose ("allow always" / "deny always"),
 * persisted per project. The live session keeps its own in-memory copies for
 * the current run, but those are wiped when a run ends or the app reloads — so
 * a standing choice would only ever hold for one run. Mirroring it here (and to
 * localStorage) makes it stick across runs and full app restarts, scoped to the
 * project it was granted in.
 *
 * Matching is exact by tool name: for MCP tools the name is the full
 * `mcp__<server>__<tool>`, so each tool is remembered individually.
 */
const STORAGE_KEY = 'devdy.toolPermissions'

interface ProjectToolPerms {
  allow: string[]
  deny: string[]
}

function load(): Record<string, ProjectToolPerms> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const obj = raw ? JSON.parse(raw) : {}
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {}
    const out: Record<string, ProjectToolPerms> = {}
    for (const [k, v] of Object.entries(obj)) {
      if (typeof k !== 'string' || !v || typeof v !== 'object') continue
      const rec = v as Record<string, unknown>
      const allow = Array.isArray(rec.allow) ? rec.allow.filter((t): t is string => typeof t === 'string') : []
      const deny = Array.isArray(rec.deny) ? rec.deny.filter((t): t is string => typeof t === 'string') : []
      if (allow.length || deny.length) out[k] = { allow, deny }
    }
    return out
  } catch {
    return {}
  }
}

export const useToolPermissionsStore = defineStore('toolPermissions', () => {
  const byProject = reactive<Record<string, ProjectToolPerms>>(load())

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...byProject }))
    } catch {
      /* storage unavailable or quota exceeded — in-memory copy still works */
    }
  }

  function entry(projectId: string): ProjectToolPerms {
    return byProject[projectId] ?? (byProject[projectId] = { allow: [], deny: [] })
  }

  function prune(projectId: string) {
    const e = byProject[projectId]
    if (e && !e.allow.length && !e.deny.length) delete byProject[projectId]
  }

  function getAllow(projectId: string): string[] {
    const e = projectId && byProject[projectId]
    return e ? [...e.allow] : []
  }

  function getDeny(projectId: string): string[] {
    const e = projectId && byProject[projectId]
    return e ? [...e.deny] : []
  }

  /** Always-allow a tool (clears any standing deny for it). */
  function allow(projectId: string, tool: string) {
    if (!projectId || !tool) return
    const e = entry(projectId)
    const d = e.deny.indexOf(tool)
    if (d !== -1) e.deny.splice(d, 1)
    if (!e.allow.includes(tool)) e.allow.push(tool)
    persist()
  }

  /** Always-deny a tool (clears any standing allow for it). */
  function deny(projectId: string, tool: string) {
    if (!projectId || !tool) return
    const e = entry(projectId)
    const a = e.allow.indexOf(tool)
    if (a !== -1) e.allow.splice(a, 1)
    if (!e.deny.includes(tool)) e.deny.push(tool)
    persist()
  }

  /** Forget any standing decision for a tool (used to revoke). */
  function reset(projectId: string, tool: string) {
    const e = byProject[projectId]
    if (!e) return
    const a = e.allow.indexOf(tool)
    if (a !== -1) e.allow.splice(a, 1)
    const d = e.deny.indexOf(tool)
    if (d !== -1) e.deny.splice(d, 1)
    prune(projectId)
    persist()
  }

  return { byProject, getAllow, getDeny, allow, deny, reset }
})
