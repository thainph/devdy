<script setup lang="ts">
// Bare, chrome-less host for the FileViewer, shown in a standalone OS window
// (opened via openFileWindow / `index.html?fileWindow=1&…`). Lets a file be read
// side-by-side with the main app window. Clicking a file link inside opens yet
// another pop-out window, keeping the "one window per file" model.
//
// `embedded`: rendered as a tab of the main window instead. The props then
// replace the URL params and a new `path` / `line` shows that file; it fills
// its parent, leaves the window's title and pin alone, uses the main window's
// dialog / toast hosts, and hands the window-level actions to the host as
// events: `close` (the file is gone), `openFile` (a link to another file) and
// `renamed` (the file moved; the view already follows it).
import { ref, onBeforeUnmount, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { ArrowDownToLine, Pin, PinOff } from 'lucide-vue-next'
import { FILE_DELETED_EVENT, FILE_MOVED_EVENT } from '@/lib/fileEvents'
import FileViewer from '@/components/FileViewer.vue'
import { Button, ConfirmModal, PromptModal, ToastHost } from '@/components/ui'
import { openFileWindow } from '@/lib/fileWindow'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { requestDock } from '@/lib/surfaceDock'
import { useDockAll } from '@/composables/useDockAll'

const props = defineProps<{
  /** A tab of the main window, not its own OS window. */
  embedded?: boolean
  /** Overrides the URL's `projectPath`. */
  projectPath?: string
  /** Overrides the URL's `path`; a new value shows that file. */
  path?: string
  /** Overrides the URL's `line`; a new value scrolls to it. */
  line?: number | null
}>()

const emit = defineEmits<{
  /** Embedded: the file was deleted, close the tab. */
  close: []
  /** Embedded: a link to another file of the project was clicked (`newWindow`: ⌘/Ctrl-click). */
  openFile: [path: string, line: number | null, newWindow: boolean]
  /** Embedded: the file was renamed / moved to `path`. */
  renamed: [path: string]
}>()

const { t } = useI18n()
// Floats above other apps while focused; pin to keep it on top deliberately.
// Embedded, the window is the main one: never float / pin it.
const { pinned, togglePin } = props.embedded ? { pinned: ref(false), togglePin: () => {} } : useFloatingWindow()

const params = new URLSearchParams(window.location.search)
const projectPath = ref(props.projectPath ?? params.get('projectPath') ?? '')
const path = ref(props.path ?? params.get('path') ?? '')
const lineParam = params.get('line')
const line = ref<number | null>(
  props.line !== undefined ? props.line : lineParam != null ? Number(lineParam) : null,
)

// The host points the tab at another file (or line).
watch(
  () => [props.projectPath, props.path, props.line] as const,
  ([nextProject, nextPath, nextLine]) => {
    if (nextProject !== undefined) projectPath.value = nextProject
    if (nextPath !== undefined) path.value = nextPath
    if (nextLine !== undefined) line.value = nextLine
  },
)

function setTitle(p: string) {
  if (props.embedded) return
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

function onOpenFile(p: string, l: number | null, newWindow: boolean) {
  if (props.embedded) emit('openFile', p, l, newWindow)
  else openFileWindow(projectPath.value, p, l)
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
  if (props.embedded) emit('renamed', next)
}

// Deleted (here or anywhere else): there is nothing left to show.
function closeWindow() {
  if (props.embedded) emit('close')
  else getCurrentWindow().close().catch(() => { /* already closing */ })
}

/** Pop-out → a tab of the main window, on the same file. */
function dock() {
  const at: Record<string, string> = { projectPath: projectPath.value, path: path.value }
  if (line.value != null) at.line = String(line.value)
  void requestDock('file', at)
}
useDockAll(dock, !props.embedded)
</script>

<template>
  <div
    class="bg-background text-foreground overflow-hidden"
    :class="embedded ? 'h-full min-h-0 w-full' : 'h-screen w-screen'"
  >
    <FileViewer
      :project-path="projectPath"
      :path="path"
      :line="line"
      @open-file="onOpenFile"
      @open-url="onOpenUrl"
      @renamed="onRenamed"
      @deleted="closeWindow"
    >
      <!-- Pop-out only. Pin: keep this viewer above other apps even after it
           loses focus; dock: move it into the main window as a tab. -->
      <template v-if="!embedded" #actions>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :class="pinned && 'bg-primary/15 text-primary hover:bg-primary/25 hover:text-primary'"
          :title="pinned ? t('files.viewer.unpin') : t('files.viewer.pin')"
          :aria-label="pinned ? t('files.viewer.unpin') : t('files.viewer.pin')"
          :aria-pressed="pinned"
          @click="togglePin"
        >
          <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :title="t('common.dockIntoMain')"
          :aria-label="t('common.dockIntoMain')"
          @click="dock"
        >
          <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </template>
    </FileViewer>

    <!-- The ⋯ menu's delete asks, its rename prompts, and both report back —
         none of which works without these hosts, and this window doesn't mount
         the main app's. -->
    <template v-if="!embedded">
      <ConfirmModal />
      <PromptModal />
      <ToastHost />
    </template>
  </div>
</template>
