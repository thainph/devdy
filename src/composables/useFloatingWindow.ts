// Window-priority behaviour shared by every pop-out (note / todo editor, file
// viewer). It answers the "these windows cover other apps" problem:
//
//   • Unpinned (default): the window floats above other apps ONLY while it has
//     focus. The moment you click into another app it drops to a normal window,
//     so a stack of pop-outs never buries whatever you switched to.
//   • Pinned: it stays on top unconditionally — for when you deliberately want a
//     note or a file watching over another app.
//
// Each pop-out is its own webview, so this runs once per window and drives that
// window alone via getCurrentWindow().
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'

export function useFloatingWindow() {
  const pinned = ref(false)
  const win = getCurrentWindow()
  let unlisten: (() => void) | null = null

  // Pinned → always on top; otherwise on top only while focused.
  async function apply(focused: boolean) {
    try {
      await win.setAlwaysOnTop(pinned.value || focused)
    } catch {
      /* window may be mid-teardown */
    }
  }

  async function isFocused(): Promise<boolean> {
    // A freshly opened / interacted-with window is focused; default to true so a
    // failed probe never wrongly sinks a window the user is looking at.
    return win.isFocused().catch(() => true)
  }

  async function togglePin() {
    pinned.value = !pinned.value
    // Pinning takes effect now; unpinning falls back to the live focus state.
    await apply(await isFocused())
  }

  onMounted(async () => {
    try {
      unlisten = await win.onFocusChanged(({ payload: focused }) => {
        apply(focused)
      })
    } catch {
      /* running outside the Tauri shell */
    }
    await apply(await isFocused())
  })

  onBeforeUnmount(() => {
    unlisten?.()
    unlisten = null
  })

  return { pinned, togglePin }
}
