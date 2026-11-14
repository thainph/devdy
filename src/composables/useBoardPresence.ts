// Whether the Batch Board is on screen. While it is, every pending permission
// prompt is answered on the board, so PermissionWindowManager holds back its
// per-run pop-up windows; they come back as soon as the board is left or the
// main window is hidden.
import { onBeforeUnmount, onMounted, ref } from 'vue'

const mounted = ref(false)
const pageVisible = ref(typeof document === 'undefined' || document.visibilityState === 'visible')

/** Read by PermissionWindowManager. */
export const boardOnScreen = () => mounted.value && pageVisible.value

/** Call from the board view's setup. */
export function useBoardPresence() {
  const onVisibility = () => {
    pageVisible.value = document.visibilityState === 'visible'
  }
  onMounted(() => {
    mounted.value = true
    onVisibility()
    document.addEventListener('visibilitychange', onVisibility)
  })
  onBeforeUnmount(() => {
    mounted.value = false
    document.removeEventListener('visibilitychange', onVisibility)
  })
}
