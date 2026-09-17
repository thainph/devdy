// App-wide channel for the todo / note list drawer — what's open and on which
// tab. Module-scope state, so every caller (the ⌘⇧K shortcut, the View menu,
// the sidebar button, the mascot menu) drives the one drawer mounted in App.vue.
//
// It holds no draft and no item: writing happens exclusively in the standalone
// item window (see lib/itemWindow), so there is nothing here to lose when the
// drawer closes.
//
// The drawer exists because reading a todo must not cost the user their working
// context: navigating to a list screen would remount the run workspace (App.vue
// keys RouterView per project, with no KeepAlive) and throw away stream scroll
// position, open panels and the viewed diff. An overlay costs none of that —
// which is also why the two list SCREENS were removed: this is the list.
import { ref } from 'vue'
import type { ItemKind } from '@/lib/itemWindow'

const open = ref(false)
const kind = ref<ItemKind>('todo')
/** The project being worked on, used to offer "this project only". */
const contextProjectId = ref<string | null>(null)

export function useItemPanel() {
  function openPanel(options: { kind?: ItemKind; projectId?: string | null } = {}) {
    if (options.kind) kind.value = options.kind
    if (options.projectId !== undefined) contextProjectId.value = options.projectId ?? null
    open.value = true
  }

  function closePanel() {
    open.value = false
  }

  function togglePanel(options: { kind?: ItemKind; projectId?: string | null } = {}) {
    // Re-pressing the shortcut on the tab already showing closes it again.
    if (open.value && (!options.kind || options.kind === kind.value)) closePanel()
    else openPanel(options)
  }

  return { open, kind, contextProjectId, openPanel, closePanel, togglePanel }
}
