// The capture draft inside the item window: which tab, the text being typed, and
// which project/run it will be filed under.
//
// The draft lives HERE rather than inside QuickCaptureForm so switching tab (or
// anything else that re-renders the form) can't throw away a half-written note —
// the fields come back exactly as they were.
//
// State is module-scope, but that scope is ONE webview: the item window is its
// own window, so its capture state is naturally isolated from the main window.
// A capture started elsewhere ("save this selection as a note") arrives as a
// prefill event and is merged through adoptDraft, which only fills fields that
// are still empty — an in-flight capture is never clobbered.
import { ref } from 'vue'

export type QuickCaptureTab = 'todo' | 'note'

/** The individual draft fields, addressable so a draft can be handed between surfaces. */
export type QuickCaptureField = 'todoText' | 'noteTitle' | 'noteContent'

export type QuickCaptureDraft = Partial<Record<QuickCaptureField, string>>

const DRAFT_FIELDS: QuickCaptureField[] = ['todoText', 'noteTitle', 'noteContent']

/** Where the capture came from; used to pre-fill the project and backlink. */
export interface QuickCaptureContext {
  projectId?: string | null
  runId?: string | null
}

export interface QuickCaptureOpenOptions {
  tab?: QuickCaptureTab
  /** Pre-filled note title (ignored on the Todo tab). */
  title?: string
  /** Pre-filled body: the todo text, or the note content. */
  content?: string
  context?: QuickCaptureContext
}

const NO_PROJECT = ''

const open = ref(false)
const tab = ref<QuickCaptureTab>('todo')

// Draft fields. Kept per tab so switching back and forth doesn't lose anything.
const todoText = ref('')
const noteTitle = ref('')
const noteContent = ref('')

// The project the capture is filed under — seeded from the opening context, then
// freely editable in the form. `runId` is the run it was captured from and is
// only ever set alongside its own project (see contextProjectId).
const projectId = ref<string>(NO_PROJECT)
const contextProjectId = ref<string | null>(null)
const runId = ref<string | null>(null)

export function useQuickCapture() {
  /**
   * Open the capture panel.
   *
   * Re-pressing the shortcut while it is already open only switches tab — it
   * must not disturb a half-typed capture. An explicitly pre-filled capture
   * (e.g. "save this selection as a note") does replace that tab's draft, since
   * that is a deliberate action on the user's part.
   */
  function openCapture(options: QuickCaptureOpenOptions = {}) {
    const hasPrefill = !!(options.title || options.content)

    if (options.tab) tab.value = options.tab
    else if (!open.value) tab.value = 'todo'

    if (!open.value || hasPrefill) {
      // A fresh open (or an explicit capture) adopts the caller's context.
      contextProjectId.value = options.context?.projectId ?? null
      runId.value = options.context?.runId ?? null
      projectId.value = contextProjectId.value ?? NO_PROJECT
    }

    if (hasPrefill) {
      if (tab.value === 'todo') {
        todoText.value = options.content ?? ''
      } else {
        noteTitle.value = options.title ?? ''
        noteContent.value = options.content ?? ''
      }
    }

    open.value = true
  }

  function closeCapture() {
    // Deliberately keeps the draft: reopening picks up where the user left off.
    open.value = false
  }

  /** Clear the fields of the tab that was just saved. */
  function clearDraft(saved: QuickCaptureTab) {
    if (saved === 'todo') {
      todoText.value = ''
    } else {
      noteTitle.value = ''
      noteContent.value = ''
    }
  }

  /** Everything typed so far, for handing the capture over to another surface. */
  function draftSnapshot(): QuickCaptureDraft {
    return {
      todoText: todoText.value,
      noteTitle: noteTitle.value,
      noteContent: noteContent.value,
    }
  }

  /**
   * Drop only the named fields — used once another surface has taken them over.
   *
   * `expected` guards the handoff round-trip: if the user reopened the panel and
   * kept typing while the pop-out was still booting, the field no longer holds
   * what was handed over and must be left alone.
   */
  function clearDraftFields(fields: QuickCaptureField[], expected?: QuickCaptureDraft) {
    const targets = { todoText, noteTitle, noteContent }
    for (const field of fields) {
      if (expected && targets[field].value !== expected[field]) continue
      targets[field].value = ''
    }
  }

  /**
   * Take over a draft handed in from another surface (the ⌘K panel popping out).
   *
   * Only fills fields that are still empty here: a draft already being typed in
   * this surface must never be clobbered. Returns the fields actually adopted so
   * the sender can clear exactly those and keep the rest.
   */
  function adoptDraft(incoming: QuickCaptureDraft): QuickCaptureField[] {
    const targets = { todoText, noteTitle, noteContent }
    const adopted: QuickCaptureField[] = []
    for (const field of DRAFT_FIELDS) {
      const value = incoming[field]
      if (!value?.trim()) continue
      if (targets[field].value.trim()) continue
      targets[field].value = value
      adopted.push(field)
    }
    return adopted
  }

  /** Point the capture at a project/run without touching the draft text. */
  function setContext(context: QuickCaptureContext) {
    contextProjectId.value = context.projectId ?? null
    runId.value = context.runId ?? null
    projectId.value = contextProjectId.value ?? NO_PROJECT
  }

  return {
    open,
    tab,
    todoText,
    noteTitle,
    noteContent,
    projectId,
    contextProjectId,
    runId,
    openCapture,
    closeCapture,
    clearDraft,
    draftSnapshot,
    clearDraftFields,
    adoptDraft,
    setContext,
  }
}
