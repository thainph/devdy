import { defineStore } from 'pinia'
import { reactive } from 'vue'

/**
 * Per-project run preferences for the composer selectors — permission mode,
 * engine and model. These were previously plain in-memory refs in RunView that
 * reset to the global default on every new run (and every app reload), so the
 * user had to re-pick "Auto-accept edits" / a specific model each time.
 * Mirroring the choices here (and to localStorage) makes them the remembered
 * default for the project, surviving new runs and full app restarts.
 *
 * An empty string means "fall back to the global default from settings".
 */
const STORAGE_KEY = 'devdy.runPrefs'

export interface RunPrefs {
  permissionMode: string
  engine: string
  model: string
}

function emptyPrefs(): RunPrefs {
  return { permissionMode: '', engine: '', model: '' }
}

function isEmpty(p: RunPrefs): boolean {
  return !p.permissionMode && !p.engine && !p.model
}

function sanitize(raw: unknown): RunPrefs | null {
  if (!raw || typeof raw !== 'object') return null
  const obj = raw as Record<string, unknown>
  const p: RunPrefs = {
    permissionMode: typeof obj.permissionMode === 'string' ? obj.permissionMode : '',
    engine: typeof obj.engine === 'string' ? obj.engine : '',
    model: typeof obj.model === 'string' ? obj.model : '',
  }
  return isEmpty(p) ? null : p
}

function load(): Record<string, RunPrefs> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const obj = raw ? JSON.parse(raw) : {}
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {}
    const out: Record<string, RunPrefs> = {}
    for (const [k, v] of Object.entries(obj)) {
      const p = sanitize(v)
      if (typeof k === 'string' && p) out[k] = p
    }
    return out
  } catch {
    return {}
  }
}

export const useRunPrefsStore = defineStore('runPrefs', () => {
  const prefs = reactive<Record<string, RunPrefs>>(load())

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...prefs }))
    } catch {
      /* storage unavailable or quota exceeded — in-memory prefs still work */
    }
  }

  function get(projectId: string): RunPrefs {
    const p = projectId && prefs[projectId]
    return p ? { ...p } : emptyPrefs()
  }

  /** Merge a partial update into a project's prefs (and clear the entry if empty). */
  function set(projectId: string, patch: Partial<RunPrefs>) {
    if (!projectId) return
    const next: RunPrefs = { ...(prefs[projectId] ?? emptyPrefs()), ...patch }
    if (isEmpty(next)) {
      if (!(projectId in prefs)) return
      delete prefs[projectId]
    } else {
      prefs[projectId] = next
    }
    persist()
  }

  return { prefs, get, set }
})
