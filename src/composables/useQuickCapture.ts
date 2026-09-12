// App-wide "quick capture" channel: the single source of truth for the Todo/Note
// capture surface — what's open, which tab, the draft being typed, and which
// project/run it should be filed under.
//
// The point is that jotting something down must NEVER cost the user their
// working context. A route change to /todos or /notes remounts RunView (its
// RouterView is keyed per project, with no KeepAlive), so the stream scroll
// position, open panels and viewed diff are all lost. Capture therefore happens
// in an overlay — QuickCapturePanel, mounted once in App.vue — which leaves the
// screen underneath completely untouched.
//
// The draft lives HERE rather than inside the form component so closing the
// panel to go re-read something doesn't throw away a half-written note: the
// fields come back exactly as they were.
//
// State is module-scope, so every caller shares one surface: the ⌘K / ⌘⇧N
// shortcuts, the mascot menu and the "save selection" button all drive the same
// panel. The standalone pop-out window is a separate webview and therefore gets
// its own independent copy of this state, which is what we want.
import { ref } from 'vue'

export type QuickCaptureTab = 'todo' | 'note'

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
    setContext,
  }
}
