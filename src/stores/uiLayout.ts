import { defineStore } from 'pinia'
import { ref } from 'vue'

/**
 * App-wide layout visibility state shared between App.vue and RunView.
 *
 * `focusMode` collapses the app down to just the current AI run: the sidebar,
 * workspace tabs, run history and issue/PR Content are hidden, leaving only
 * Claude's output/question area and the composer. It is intentionally a
 * transient, in-memory flag — it resets on reload rather than persisting.
 */
export const useUILayoutStore = defineStore('uiLayout', () => {
  const focusMode = ref(false)

  function toggleFocus() {
    focusMode.value = !focusMode.value
  }

  return { focusMode, toggleFocus }
})
