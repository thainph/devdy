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
 * Row density shared by the session lists — the History list (RunView's left
 * rail) and the worker panel. Persisted like `sidebarHidden`, since it is a
 * lasting reading preference.
 *  - comfortable: the full multi-line row (badges + timestamp line)
 *  - compact:     two lines — title row + "account · model"
 *  - dense:       one line; the rest lives in the row tooltip
 */
export type ListDensity = 'comfortable' | 'compact' | 'dense'
const LIST_DENSITY_KEY = 'devdy.listDensity'
// Pre-rename key (History-only density); read once so the choice carries over.
const LEGACY_HISTORY_DENSITY_KEY = 'devdy.historyDensity'
const LIST_DENSITIES: readonly ListDensity[] = ['comfortable', 'compact', 'dense']

function loadListDensity(): ListDensity {
  try {
    let v = localStorage.getItem(LIST_DENSITY_KEY)
    if (v == null) {
      v = localStorage.getItem(LEGACY_HISTORY_DENSITY_KEY)
      if (v != null) {
        localStorage.setItem(LIST_DENSITY_KEY, v)
        localStorage.removeItem(LEGACY_HISTORY_DENSITY_KEY)
      }
    }
    if (v && (LIST_DENSITIES as readonly string[]).includes(v)) return v as ListDensity
  } catch {
    /* storage unavailable — fall through to the default */
  }
  return 'compact'
}

/**
 * Worker-panel timeline: collapsed to a one-line header by default; when open,
 * its height (px) is user-resizable. Both persist across restarts.
 */
const TIMELINE_OPEN_KEY = 'devdy.workerTimelineOpen'
const TIMELINE_HEIGHT_KEY = 'devdy.workerTimelineHeight'
export const TIMELINE_MIN_HEIGHT = 80
const TIMELINE_DEFAULT_HEIGHT = 192

function loadTimelineOpen(): boolean {
  try {
    return localStorage.getItem(TIMELINE_OPEN_KEY) === '1'
  } catch {
    return false
  }
}

function loadTimelineHeight(): number {
  try {
    const n = Number(localStorage.getItem(TIMELINE_HEIGHT_KEY))
    if (Number.isFinite(n) && n >= TIMELINE_MIN_HEIGHT) return Math.round(n)
  } catch {
    /* storage unavailable — fall through to the default */
  }
  return TIMELINE_DEFAULT_HEIGHT
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

  const listDensity = ref<ListDensity>(loadListDensity())

  function setListDensity(density: ListDensity) {
    listDensity.value = density
    try {
      localStorage.setItem(LIST_DENSITY_KEY, density)
    } catch {
      /* storage unavailable — the in-memory value still works for this session */
    }
  }

  const timelineOpen = ref(loadTimelineOpen())
  const timelineHeight = ref(loadTimelineHeight())

  function setTimelineOpen(open: boolean) {
    timelineOpen.value = open
    try {
      if (open) localStorage.setItem(TIMELINE_OPEN_KEY, '1')
      else localStorage.removeItem(TIMELINE_OPEN_KEY)
    } catch {
      /* storage unavailable — the in-memory flag still works for this session */
    }
  }

  /** Callers clamp the upper bound (it depends on the panel's height). */
  function setTimelineHeight(px: number) {
    timelineHeight.value = Math.max(TIMELINE_MIN_HEIGHT, Math.round(px))
    try {
      localStorage.setItem(TIMELINE_HEIGHT_KEY, String(timelineHeight.value))
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
    listDensity,
    setListDensity,
    timelineOpen,
    setTimelineOpen,
    timelineHeight,
    setTimelineHeight,
  }
})
