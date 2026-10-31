/**
 * Todo / Note / project-switch state for the Controller ({@link FEATURE_DATA}).
 *
 * Kept apart from `runStore` (which is about runs) so the data screens are
 * self-contained. Reads land through {@link DataStore.apply}; writes go out with
 * a delivery ack and resolve to whether the Host accepted them, so a sheet or
 * editor only closes once its change is actually saved.
 *
 * The Host pushes the fresh `todo_list` after every todo write and the real
 * `project_toggles` after every switch, so those need no re-fetch. Note writes
 * are not followed by a push: the store re-lists with the current filter.
 */
import { reactive } from 'vue'
import type { CommandAckHandler } from './connection'
import {
  noteAddCmd,
  noteDeleteCmd,
  noteGetCmd,
  noteListCmd,
  noteUpdateCmd,
  projectTogglesGetCmd,
  projectToggleSetCmd,
  todoAddCmd,
  todoDeleteCmd,
  todoListCmd,
  todoToggleCmd,
  todoUpdateCmd,
  type CmdPayload,
  type NoteInfo,
  type NoteSummary,
  type NotePatchWire,
  type ProjectTogglesKind,
  type StreamPayload,
  type TodoInfo,
  type TodoPatchWire,
  type ToggleItem,
  type ToggleKind,
} from './protocol.ts'

/** `ControllerConnection.sendCommand`; false when the room is not live. */
export type SendCommand = (cmd: CmdPayload, onAck?: CommandAckHandler) => boolean

/** What the todo edit sheet saves. `id` null = a new todo. */
export interface TodoSave {
  id: string | null
  text: string
  dueDate: string
  projectId: string
  /** The todo as the sheet opened it: only fields changed since then are sent,
   * so a field the user did not touch never overwrites a desktop edit. */
  base?: TodoInfo | null
}

/** What the note editor saves. `id` null = a new note. */
export interface NoteSave {
  id: string | null
  title: string
  content: string
  projectId: string
  /** The note as the editor loaded it (see {@link TodoSave.base}). */
  base?: NoteInfo | null
}

/** Which reads failed (refused, or the Host never answered): the screen shows
 * an error with a retry instead of spinning forever. */
export interface LoadFailures {
  todos: boolean
  notes: boolean
  toggles: boolean
  note: boolean
}

export interface DataState {
  /** null until the first `todo_list` arrives. */
  todos: TodoInfo[] | null
  notes: NoteSummary[] | null
  /** The full note the editor asked for (`note_get`); null while loading. */
  note: NoteInfo | null
  /** The id of that note. */
  noteId: string | null
  /** The filter of the last note request; replies for another one are dropped. */
  noteFilter: { projectId: string | null; query: string }
  toggles: ProjectTogglesKind | null
  /** The project whose switches were last asked for. */
  togglesProjectId: string | null
  /** Writes in flight. */
  pending: number
  /** The last refused / undelivered write, already human-readable. */
  error: string | null
  failed: LoadFailures
}

export function createDataStore(send: SendCommand, describeError: (reason: string | null) => string) {
  const state = reactive<DataState>({
    todos: null,
    notes: null,
    note: null,
    noteId: null,
    noteFilter: { projectId: null, query: '' },
    toggles: null,
    togglesProjectId: null,
    pending: 0,
    error: null,
    failed: { todos: false, notes: false, toggles: false, note: false },
  })

  function apply(p: StreamPayload): void {
    switch (p.kind) {
      case 'todo_list':
        state.todos = p.todos
        break
      case 'note_list':
        // A reply to a filter the user has since changed would flash stale rows.
        if (
          (p.project_id ?? null) === state.noteFilter.projectId &&
          (p.query ?? '') === state.noteFilter.query
        ) {
          state.notes = p.notes
        }
        break
      case 'project_toggles':
        if (p.project_id === state.togglesProjectId) state.toggles = p
        break
      case 'note':
        if (p.note.id === state.noteId) state.note = p.note
        break
    }
  }

  /** Send a read. Its answer arrives as a payload (see {@link apply}) before
   * the ack; a refusal or a missing ack marks `which` failed so the screen
   * stops spinning. */
  function read(cmd: CmdPayload, which: keyof LoadFailures, stillWanted: () => boolean = () => true): void {
    state.failed[which] = false
    const sent = send(cmd, (accepted, reason) => {
      if (accepted || !stillWanted()) return
      state.failed[which] = true
      state.error = describeError(reason === 'host_ack_timeout' ? 'load_timeout' : (reason ?? null))
    })
    if (!sent) {
      state.failed[which] = true
      state.error = describeError('not_connected')
    }
  }

  /** Send a write and resolve to whether the Host accepted it. */
  async function write(cmd: CmdPayload): Promise<boolean> {
    return (await writeAck(cmd)).ok
  }

  /** {@link write}, also yielding the id an add created. */
  function writeAck(cmd: CmdPayload): Promise<{ ok: boolean; itemId: string | null }> {
    state.error = null
    return new Promise((resolve) => {
      const sent = send(cmd, (accepted, reason, _runId, itemId) => {
        state.pending = Math.max(0, state.pending - 1)
        if (!accepted) state.error = describeError(reason ?? null)
        resolve({ ok: accepted, itemId: itemId ?? null })
      })
      if (sent) {
        state.pending += 1
      } else {
        state.error = describeError('not_connected')
        resolve({ ok: false, itemId: null })
      }
    })
  }

  // ---- todos ----

  function loadTodos(): void {
    read(todoListCmd(), 'todos')
  }

  async function saveTodo(input: TodoSave): Promise<boolean> {
    if (!input.id) {
      return write(todoAddCmd({ text: input.text, projectId: input.projectId, dueDate: input.dueDate }))
    }
    // Diff against what the sheet opened with — not the live list, which may
    // already carry a desktop edit the user never saw.
    const current = input.base ?? null
    const patch: TodoPatchWire = {}
    if (!current || current.text !== input.text) patch.text = input.text
    if (!current || (current.due_date ?? '') !== input.dueDate) patch.due_date = input.dueDate || null
    if (!current || (current.project_id ?? '') !== input.projectId) patch.project_id = input.projectId || null
    if (Object.keys(patch).length === 0) return true
    return write(todoUpdateCmd(input.id, patch))
  }

  /** Flip done optimistically; the Host's pushed list is the truth either way. */
  function toggleTodo(id: string): void {
    const t = state.todos?.find((x) => x.id === id)
    if (t) t.done = !t.done
    void write(todoToggleCmd(id)).then((ok) => {
      if (!ok) loadTodos()
    })
  }

  function deleteTodo(id: string): Promise<boolean> {
    return write(todoDeleteCmd(id))
  }

  // ---- notes ----

  function loadNotes(projectId: string | null = state.noteFilter.projectId, query = state.noteFilter.query): void {
    const filter = { projectId, query: query.trim() }
    state.noteFilter = filter
    read(
      noteListCmd(projectId, query),
      'notes',
      () => state.noteFilter.projectId === filter.projectId && state.noteFilter.query === filter.query,
    )
  }

  /** Fetch one full note for the editor (the list only carries previews). */
  function loadNote(id: string): void {
    if (state.noteId !== id) state.note = null
    state.noteId = id
    read(noteGetCmd(id), 'note', () => state.noteId === id)
  }

  /** The editor closed (or moved to a new note). */
  function closeNote(): void {
    state.noteId = null
    state.note = null
    state.failed.note = false
  }

  /** Save a note; resolves to the note as stored (null when refused), so the
   * editor can show a just-created note under its new id. */
  async function saveNote(input: NoteSave): Promise<NoteInfo | null> {
    const saved = (id: string): NoteInfo => ({
      id,
      title: input.title.trim(),
      content: input.content.trim(),
      project_id: input.projectId || null,
      updated_at: new Date().toISOString(),
    })
    if (!input.id) {
      const { ok, itemId } = await writeAck(
        noteAddCmd({ title: input.title, content: input.content, projectId: input.projectId }),
      )
      if (!ok) return null
      loadNotes()
      // An id-less ack (an older Host) still saved it: '' sends the editor back to the list.
      const stored = saved(itemId ?? '')
      if (itemId) {
        state.noteId = itemId
        state.note = stored
      }
      return stored
    }
    const current = input.base ?? null
    const patch: NotePatchWire = {}
    if (!current || current.title !== input.title) patch.title = input.title
    if (!current || current.content !== input.content) patch.content = input.content
    if (!current || (current.project_id ?? '') !== input.projectId) patch.project_id = input.projectId || null
    if (Object.keys(patch).length === 0) return saved(input.id)
    if (!(await write(noteUpdateCmd(input.id, patch)))) return null
    loadNotes()
    const stored = saved(input.id)
    if (state.noteId === input.id) state.note = stored
    return stored
  }

  async function deleteNote(id: string): Promise<boolean> {
    const ok = await write(noteDeleteCmd(id))
    if (ok) loadNotes()
    return ok
  }

  // ---- project switches ----

  function loadToggles(projectId: string): void {
    if (state.togglesProjectId !== projectId) state.toggles = null
    state.togglesProjectId = projectId
    read(projectTogglesGetCmd(projectId), 'toggles', () => state.togglesProjectId === projectId)
  }

  /** Flip a switch optimistically. The Host answers with the real state — a
   * skill a group still brings in stays on — and a refusal re-fetches it. */
  function setToggle(kind: ToggleKind, itemId: string | null, enabled: boolean): void {
    const projectId = state.togglesProjectId
    const tg = state.toggles
    if (!projectId || !tg) return
    if (kind === 'builtin_mcp') {
      tg.builtin_devdy.enabled = enabled
    } else if (kind === 'claude_account') {
      // One linked account at most: switching one on switches the rest off.
      for (const a of tg.claude_accounts) {
        if (a.id === itemId) a.enabled = enabled
        else if (enabled) a.enabled = false
      }
    } else {
      const lists: Record<Exclude<ToggleKind, 'builtin_mcp' | 'claude_account'>, ToggleItem[]> = {
        skill: tg.skills,
        rule: tg.rules,
        mcp: tg.mcp_servers,
        server: tg.servers,
        github_account: tg.github_accounts,
        gitlab_account: tg.gitlab_accounts,
        aws_account: tg.aws_accounts,
        azure_account: tg.azure_accounts,
      }
      const item = lists[kind].find((i) => i.id === itemId)
      if (item) item.enabled = enabled
    }
    void write(projectToggleSetCmd(projectId, kind, itemId, enabled)).then((ok) => {
      // Only if the user is still on that project: re-fetching another one
      // would yank the screen back to it.
      if (!ok && state.togglesProjectId === projectId) loadToggles(projectId)
    })
  }

  /** After a reconnect: re-fetch whatever a screen has already loaded. */
  function reload(): void {
    if (state.todos) loadTodos()
    if (state.notes) loadNotes()
    if (state.togglesProjectId) loadToggles(state.togglesProjectId)
    if (state.noteId) loadNote(state.noteId)
  }

  function clearError(): void {
    state.error = null
  }

  function reset(): void {
    state.todos = null
    state.notes = null
    state.note = null
    state.noteId = null
    state.failed = { todos: false, notes: false, toggles: false, note: false }
    state.noteFilter = { projectId: null, query: '' }
    state.toggles = null
    state.togglesProjectId = null
    state.pending = 0
    state.error = null
  }

  return {
    state,
    apply,
    loadTodos,
    saveTodo,
    toggleTodo,
    deleteTodo,
    loadNotes,
    loadNote,
    closeNote,
    saveNote,
    deleteNote,
    loadToggles,
    setToggle,
    reload,
    clearError,
    reset,
  }
}

export type DataStore = ReturnType<typeof createDataStore>
