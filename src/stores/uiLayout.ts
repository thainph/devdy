import { defineStore } from 'pinia'
import { ref } from 'vue'

/**
 * App-wide layout visibility state shared between App.vue and RunView.
 *
 * `focusMode` collapses the app down to just the current AI run: the sidebar,
 * workspace tabs, run history and issue/PR Content are hidden, leaving only
 * Claude's output/question area and the composer. It is intentionally a
 * transient, in-memory flag — it resets on reload rather than persisting.
 *
 * `sidebarHidden` is the separate, deliberate "give me the full width on every
 * screen" toggle (⌘/Ctrl+B). Unlike focus mode it applies to ALL routes and IS
 * persisted, because someone who works with the nav hidden expects it to stay
 * hidden across restarts rather than to reappear on every launch.
 */
const SIDEBAR_STORAGE_KEY = 'devdy.sidebarHidden'

function loadSidebarHidden(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_STORAGE_KEY) === '1'
  } catch {
    return false
  }
}

export const useUILayoutStore = defineStore('uiLayout', () => {
  const focusMode = ref(false)
  const sidebarHidden = ref(loadSidebarHidden())

  function toggleFocus() {
    focusMode.value = !focusMode.value
  }

  function setSidebarHidden(hidden: boolean) {
    sidebarHidden.value = hidden
    try {
      if (hidden) localStorage.setItem(SIDEBAR_STORAGE_KEY, '1')
      else localStorage.removeItem(SIDEBAR_STORAGE_KEY)
    } catch {
      /* storage unavailable — the in-memory flag still works for this session */
    }
  }

  function toggleSidebar() {
    setSidebarHidden(!sidebarHidden.value)
  }

  return { focusMode, toggleFocus, sidebarHidden, setSidebarHidden, toggleSidebar }
})
