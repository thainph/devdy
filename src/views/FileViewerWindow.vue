<script setup lang="ts">
// Bare, chrome-less host for the FileViewer, shown in a standalone OS window
// (opened via openFileWindow / `index.html?fileWindow=1&…`). Lets a file be read
// side-by-side with the main app window. Clicking a file link inside opens yet
// another pop-out window, keeping the "one window per file" model.
import { ref, onMounted } from 'vue'
import { openUrl } from '@tauri-apps/plugin-opener'
import FileViewer from '@/components/FileViewer.vue'
import { openFileWindow } from '@/lib/fileWindow'

const params = new URLSearchParams(window.location.search)
const projectPath = ref(params.get('projectPath') ?? '')
const path = ref(params.get('path') ?? '')
const lineParam = params.get('line')
const line = ref<number | null>(lineParam != null ? Number(lineParam) : null)

onMounted(() => {
  document.title = (path.value.split('/').pop() || path.value) + ' — Devdy'
})

function onOpenFile(p: string, l: number | null) {
  openFileWindow(projectPath.value, p, l)
}
function onOpenUrl(url: string) {
  openUrl(url).catch(() => { /* opener unavailable */ })
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
    />
  </div>
</template>
