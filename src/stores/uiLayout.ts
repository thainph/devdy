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

/**
 * Row density of the session History list (RunView's left rail). Persisted like
 * `sidebarHidden`, since it is a lasting reading preference.
 *  - comfortable: the full multi-line row (badges + timestamp line)
 *  - compact:     two lines — title row + "account · model"
 *  - dense:       one line; the rest lives in the row tooltip
 */
export type HistoryDensity = 'comfortable' | 'compact' | 'dense'
const HISTORY_DENSITY_KEY = 'devdy.historyDensity'
const HISTORY_DENSITIES: readonly HistoryDensity[] = ['comfortable', 'compact', 'dense']

function loadHistoryDensity(): HistoryDensity {
  try {
    const v = localStorage.getItem(HISTORY_DENSITY_KEY)
    if (v && (HISTORY_DENSITIES as readonly string[]).includes(v)) return v as HistoryDensity
  } catch {
    /* storage unavailable — fall through to the default */
  }
  return 'compact'
}

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

  const historyDensity = ref<HistoryDensity>(loadHistoryDensity())

  function setHistoryDensity(density: HistoryDensity) {
    historyDensity.value = density
    try {
      localStorage.setItem(HISTORY_DENSITY_KEY, density)
    } catch {
      /* storage unavailable — the in-memory value still works for this session */
    }
  }

  return {
    focusMode,
    toggleFocus,
    sidebarHidden,
    setSidebarHidden,
    toggleSidebar,
    historyDensity,
    setHistoryDensity,
  }
})
