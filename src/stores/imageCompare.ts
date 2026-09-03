// Coordinates the "compare two images side by side" feature across the app.
//
// Flow:
//   1. Pick the first image — from the image viewer's Compare button or the file
//      tree context menu ("Compare with…"). This sets `pendingFirst` and shows a
//      floating banner prompting for the second image.
//   2. Pick the second image — either right-click another image in the tree
//      ("Compare with '<first>'") or choose one from the project image picker.
//   3. The compare view opens with both images and a shared zoom/pan state.
//
// A single <ImageCompareHost> (mounted once in App.vue's main window) renders the
// banner, picker and compare view off this store, so any component can trigger a
// comparison without prop-drilling.
import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { useRunsStore } from './runs'

const IMAGE_EXT = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico', 'avif', 'apng']

/** True when the path points at a previewable raster/vector image. */
export function isImagePath(path: string): boolean {
  const base = (path.split('/').pop() ?? path).toLowerCase()
  const ext = base.includes('.') ? base.slice(base.lastIndexOf('.') + 1) : ''
  return IMAGE_EXT.includes(ext)
}

function basename(p: string): string {
  return p.split('/').pop() ?? p
}

export const useImageCompareStore = defineStore('imageCompare', () => {
  const runs = useRunsStore()

  const projectPath = ref('')
  const pendingFirst = ref('') // relative path of the first image awaiting a second
  const left = ref('')
  const right = ref('')
  const open = ref(false) // compare view open
  const pickerOpen = ref(false) // second-image picker modal open
  const images = ref<string[]>([]) // project image paths for the picker
  const loadingImages = ref(false)

  const hasPending = computed(() => !!pendingFirst.value)
  const pendingName = computed(() => basename(pendingFirst.value))

  /** Step 1 — remember the first image; a second pick completes the comparison. */
  function selectFirst(proj: string, path: string) {
    projectPath.value = proj
    pendingFirst.value = path
  }

  /** Step 2a — pair the pending first image with a second and open the view. */
  function selectSecond(path: string) {
    if (!pendingFirst.value || path === pendingFirst.value) return
    left.value = pendingFirst.value
    right.value = path
    pendingFirst.value = ''
    pickerOpen.value = false
    open.value = true
  }

  /** Step 2b — open the project image picker (loads the image list lazily). */
  async function openPicker() {
    pickerOpen.value = true
    await loadImages()
  }

  async function loadImages() {
    if (!projectPath.value) return
    loadingImages.value = true
    try {
      const entries = await runs.listProjectFiles(projectPath.value)
      images.value = entries
        .filter((e) => !e.is_dir && isImagePath(e.path))
        .map((e) => e.path)
        .sort((a, b) => a.localeCompare(b))
    } catch {
      images.value = []
    } finally {
      loadingImages.value = false
    }
  }

  function cancelPending() {
    pendingFirst.value = ''
    pickerOpen.value = false
  }

  function swap() {
    const t = left.value
    left.value = right.value
    right.value = t
  }

  function close() {
    open.value = false
    left.value = ''
    right.value = ''
  }

  return {
    projectPath,
    pendingFirst,
    left,
    right,
    open,
    pickerOpen,
    images,
    loadingImages,
    hasPending,
    pendingName,
    selectFirst,
    selectSecond,
    openPicker,
    loadImages,
    cancelPending,
    swap,
    close,
  }
})
