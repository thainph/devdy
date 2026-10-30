import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'
import type { AddTodoInput } from '@/lib/focus'

/**
 * Global "quick note" todo list, persisted in the app SQLite DB (todos table)
 * via Tauri commands. Sessions (runs) are linked many-to-many (`todo_runs`, see
 * lib/sessionLinks). Array order mirrors the `position` column — drag-and-drop
 * in the view reorders locally then persists the new id order.
 *
 * Mutations apply optimistically for a snappy UI and refetch on error to resync.
 */
export interface Todo {
  id: string
  text: string
  done: boolean
  position: number
  created_at: string
  /** Capture context: the project this was jotted down in, when any. */
  project_id: string | null
  /** Deadline, a local `YYYY-MM-DD` date; null = none. */
  due_date: string | null
  /** Linked sessions whose run still exists. */
  linked_run_count: number
}

export const useTodosStore = defineStore('todos', () => {
  const todos = ref<Todo[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetchTodos() {
    loading.value = true
    error.value = null
    try {
      todos.value = await invoke<Todo[]>('list_todos')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  /** `extra` carries the Focus fields of the "+ Todo" form (deadline time, priority, estimate, source). */
  async function add(
    text: string,
    projectId?: string | null,
    runId?: string | null,
    extra: Omit<AddTodoInput, 'text' | 'projectId' | 'runId'> = {},
  ) {
    const trimmed = text.trim()
    if (!trimmed) return
    try {
      const todo = await invoke<Todo>('add_todo', {
        ...extra,
        text: trimmed,
        projectId: projectId ?? null,
        // The session it was written in becomes a `captured` link.
        runId: runId ?? null,
      })
      todos.value.unshift(todo)
      return todo
    } catch (e) {
      error.value = String(e)
      await fetchTodos()
    }
  }

  /** Link or unlink a todo from a project (session links are independent of it). */
  async function setProject(id: string, projectId: string | null) {
    const todo = todos.value.find(t => t.id === id)
    const prev = todo ? todo.project_id : null
    if (todo) todo.project_id = projectId // optimistic
    try {
      await invoke('set_todo_project', { id, projectId })
    } catch (e) {
      error.value = String(e)
      if (todo) todo.project_id = prev
    }
  }

  /** Set or clear (null) a todo's deadline, a `YYYY-MM-DD` date. */
  async function setDueDate(id: string, dueDate: string | null) {
    const todo = todos.value.find(t => t.id === id)
    const prev = todo ? todo.due_date : null
    if (todo) todo.due_date = dueDate // optimistic
    try {
      await invoke('set_todo_due_date', { id, dueDate })
    } catch (e) {
      error.value = String(e)
      if (todo) todo.due_date = prev
    }
  }

  async function toggle(id: string) {
    const t = todos.value.find(t => t.id === id)
    if (!t) return
    t.done = !t.done // optimistic
    try {
      await invoke('toggle_todo', { id })
    } catch (e) {
      error.value = String(e)
      await fetchTodos()
    }
  }

  async function update(id: string, text: string) {
    const trimmed = text.trim()
    if (!trimmed) {
      await remove(id)
      return
    }
    const t = todos.value.find(t => t.id === id)
    const prev = t?.text
    if (t) t.text = trimmed // optimistic
    try {
      await invoke('update_todo', { id, text: trimmed })
    } catch (e) {
      error.value = String(e)
      if (t && prev !== undefined) t.text = prev
    }
  }

  async function remove(id: string) {
    const i = todos.value.findIndex(t => t.id === id)
    if (i === -1) return
    const [removed] = todos.value.splice(i, 1) // optimistic
    try {
      await invoke('delete_todo', { id })
    } catch (e) {
      error.value = String(e)
      todos.value.splice(i, 0, removed)
    }
  }

  /** Bulk-delete todos by id (optimistic), refetching to resync on error. */
  async function removeMany(ids: string[]) {
    if (ids.length === 0) return
    const target = new Set(ids)
    const snapshot = todos.value.slice()
    todos.value = todos.value.filter(t => !target.has(t.id)) // optimistic
    try {
      await invoke('delete_todos', { ids })
    } catch (e) {
      error.value = String(e)
      todos.value = snapshot
    }
  }

  async function clearCompleted() {
    const snapshot = todos.value.slice()
    todos.value = todos.value.filter(t => !t.done) // optimistic
    try {
      await invoke('clear_completed_todos')
    } catch (e) {
      error.value = String(e)
      todos.value = snapshot
    }
  }

  /** Move the todo at `from` to `to` (drag-and-drop reorder), then persist. */
  async function reorder(from: number, to: number) {
    if (from === to) return
    if (from < 0 || from >= todos.value.length) return
    if (to < 0 || to >= todos.value.length) return
    const snapshot = todos.value.slice()
    const [moved] = todos.value.splice(from, 1)
    todos.value.splice(to, 0, moved)
    try {
      await invoke('reorder_todos', { ids: todos.value.map(t => t.id) })
    } catch (e) {
      error.value = String(e)
      todos.value = snapshot
    }
  }

  return { todos, loading, error, fetchTodos, add, setProject, setDueDate, toggle, update, remove, removeMany, clearCompleted, reorder }
})
