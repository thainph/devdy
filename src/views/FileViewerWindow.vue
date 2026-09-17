<script setup lang="ts">
// Bare, chrome-less host for the FileViewer, shown in a standalone OS window
// (opened via openFileWindow / `index.html?fileWindow=1&…`). Lets a file be read
// side-by-side with the main app window. Clicking a file link inside opens yet
// another pop-out window, keeping the "one window per file" model.
import { ref, onBeforeUnmount, onMounted } from 'vue'
import { openUrl } from '@tauri-apps/plugin-opener'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { FILE_DELETED_EVENT, FILE_MOVED_EVENT } from '@/lib/fileEvents'
import FileViewer from '@/components/FileViewer.vue'
import { ConfirmModal, PromptModal, ToastHost } from '@/components/ui'
import { openFileWindow } from '@/lib/fileWindow'

const params = new URLSearchParams(window.location.search)
const projectPath = ref(params.get('projectPath') ?? '')
const path = ref(params.get('path') ?? '')
const lineParam = params.get('line')
const line = ref<number | null>(lineParam != null ? Number(lineParam) : null)

function setTitle(p: string) {
  document.title = (p.split('/').pop() || p) + ' — Devdy'
}

/** True when `candidate` IS our file, or the folder our file lives in. */
function covers(candidate: string): boolean {
  return path.value === candidate || path.value.startsWith(`${candidate}/`)
}

let unlisten: UnlistenFn[] = []

onMounted(async () => {
  setTitle(path.value)

  // This window shows ONE path and has no other way to learn that the path
  // stopped being valid — the explorer that deleted or renamed it is a different
  // webview. So both are announced app-wide and acted on here.
  try {
    unlisten.push(
      await listen<{ projectPath: string; path: string }>(FILE_DELETED_EVENT, (e) => {
        const payload = e.payload
        if (!payload || payload.projectPath !== projectPath.value) return
        // Deleting the folder takes everything under it, this file included.
        if (covers(payload.path)) closeWindow()
      }),
      await listen<{ projectPath: string; from: string; to: string }>(FILE_MOVED_EVENT, (e) => {
        const payload = e.payload
        if (!payload || payload.projectPath !== projectPath.value) return
        if (path.value === payload.from) onRenamed(payload.to)
        else if (path.value.startsWith(`${payload.from}/`)) {
          onRenamed(payload.to + path.value.slice(payload.from.length))
        }
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
})

onBeforeUnmount(() => {
  unlisten.forEach((off) => off())
  unlisten = []
})

function onOpenFile(p: string, l: number | null) {
  openFileWindow(projectPath.value, p, l)
}
function onOpenUrl(url: string) {
  openUrl(url).catch(() => { /* opener unavailable */ })
}

// Renamed or moved (here or anywhere else): follow the file rather than sit on
// a path that no longer exists.
function onRenamed(next: string) {
  path.value = next
  line.value = null
  setTitle(next)
}

// Deleted (here or anywhere else): there is nothing left to show.
function closeWindow() {
  getCurrentWindow().close().catch(() => { /* already closing */ })
}
</script>

<template>
  <div class="h-screen w-screen bg-background text-foreground overflow-hidden">
    <FileViewer
      :project-path="projectPath"
      :path="path"
      :line="line"
      @open-file="onOpenFile"
      @open-url="onOpenUrl"
      @renamed="onRenamed"
      @deleted="closeWindow"
    />

    <!-- The ⋯ menu's delete asks, its rename prompts, and both report back —
         none of which works without these hosts, and this window doesn't mount
         the main app's. -->
    <ConfirmModal />
    <PromptModal />
    <ToastHost />
  </div>
</template>
