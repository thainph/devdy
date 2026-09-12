import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

/**
 * Global quick-notes scratchpad, persisted in the app SQLite DB (notes table)
 * via Tauri commands. Each note has a short title + markdown content and may
 * optionally be linked to a project (`project_id`). Array order mirrors the
 * `position` column — drag-and-drop in the view reorders locally then persists
 * the new id order.
 *
 * Mutations apply optimistically for a snappy UI and refetch on error to resync.
 */
export interface Note {
  id: string
  title: string
  content: string
  project_id: string | null
  /** Capture context: the AI run it came from, for the backlink. */
  run_id: string | null
  position: number
  created_at: string
  updated_at: string
}

export const useNotesStore = defineStore('notes', () => {
  const notes = ref<Note[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetchNotes(projectId?: string | null) {
    loading.value = true
    error.value = null
    try {
      notes.value = await invoke<Note[]>('list_notes', { projectId: projectId ?? null })
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function add(
    title: string,
    content: string,
    projectId?: string | null,
    runId?: string | null,
  ) {
    const t = title.trim()
    const c = content.trim()
    if (!t && !c) return
    try {
      const note = await invoke<Note>('add_note', {
        title: t,
        content: c,
        projectId: projectId ?? null,
        // A run backlink only means anything alongside its project.
        runId: projectId ? runId ?? null : null,
      })
      notes.value.unshift(note)
      return note
    } catch (e) {
      error.value = String(e)
      await fetchNotes()
    }
  }

  async function update(id: string, title: string, content: string) {
    const t = title.trim()
    const c = content.trim()
    if (!t && !c) return
    const n = notes.value.find(n => n.id === id)
    const prev = n ? { title: n.title, content: n.content } : null
    if (n) {
      n.title = t
      n.content = c // optimistic
    }
    try {
      await invoke('update_note', { id, title: t, content: c })
      if (n) n.updated_at = new Date().toISOString()
    } catch (e) {
      error.value = String(e)
      if (n && prev) {
        n.title = prev.title
        n.content = prev.content
      }
    }
  }

  async function setProject(id: string, projectId: string | null) {
    const n = notes.value.find(n => n.id === id)
    const prev = n ? { projectId: n.project_id, runId: n.run_id } : null
    if (n) {
      n.project_id = projectId // optimistic
      if (!projectId) n.run_id = null // a run backlink can't outlive its project link
    }
    try {
      await invoke('set_note_project', { id, projectId })
    } catch (e) {
      error.value = String(e)
      if (n && prev) {
        n.project_id = prev.projectId
        n.run_id = prev.runId
      }
    }
  }

  async function remove(id: string) {
    const i = notes.value.findIndex(n => n.id === id)
    if (i === -1) return
    const [removed] = notes.value.splice(i, 1) // optimistic
    try {
      await invoke('delete_note', { id })
    } catch (e) {
      error.value = String(e)
      notes.value.splice(i, 0, removed)
    }
  }

  /** Bulk-delete notes by id (optimistic), refetching to resync on error. */
  async function removeMany(ids: string[]) {
    if (ids.length === 0) return
    const target = new Set(ids)
    const snapshot = notes.value.slice()
    notes.value = notes.value.filter(n => !target.has(n.id)) // optimistic
    try {
      await invoke('delete_notes', { ids })
    } catch (e) {
      error.value = String(e)
      notes.value = snapshot
    }
  }

  /** Move the note at `from` to `to` (drag-and-drop reorder), then persist. */
  async function reorder(from: number, to: number) {
    if (from === to) return
    if (from < 0 || from >= notes.value.length) return
    if (to < 0 || to >= notes.value.length) return
    const snapshot = notes.value.slice()
    const [moved] = notes.value.splice(from, 1)
    notes.value.splice(to, 0, moved)
    try {
      await invoke('reorder_notes', { ids: notes.value.map(n => n.id) })
    } catch (e) {
      error.value = String(e)
      notes.value = snapshot
    }
  }

  return { notes, loading, error, fetchNotes, add, update, setProject, remove, removeMany, reorder }
})
