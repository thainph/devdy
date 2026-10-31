<script setup lang="ts">
// THE File Transfer window (opened via lib/fileTransferWindow, `?fileTransferWindow=1`,
// remote set only): this machine on the left, the controlled Devdy on the
// right, a transfer queue below. Copy with the middle buttons, by dragging rows
// to the other pane, or by dropping files from Finder onto the right pane.
//
// Planning (walking folders, asking about clashes, creating the folder tree)
// and the copies themselves live in lib/fileTransferCore.ts; the commands of
// both machines in lib/fileTransfer.ts.
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { ArrowLeft, ArrowRight, Loader2 } from 'lucide-vue-next'
import { Button, ConfirmModal, PromptModal, ToastHost } from '@/components/ui'
import FileTransferPane from '@/components/fileTransfer/FileTransferPane.vue'
import FileTransferQueue from '@/components/fileTransfer/FileTransferQueue.vue'
import FileTransferConflict from '@/components/fileTransfer/FileTransferConflict.vue'
import { useToast } from '@/composables/useToast'
import { refreshTray } from '@/lib/tray'
import { nativeDropPoint } from '@/lib/fileTreeDnd'
import { loadRemoteDesktopSession } from '@/lib/remoteDesktopSession'
import type { FsEntry } from '@/lib/ipc/nativeCore'
import {
  ConflictResolver,
  TransferQueue,
  planTransfer,
  type ConflictAnswer,
  type Direction,
} from '@/lib/fileTransferCore'
import { destSide, droppedEntries, localPaneFs, paneFs, remotePaneFs, transferIo, type Side } from '@/lib/fileTransfer'

const { t, locale } = useI18n()
const { toast } = useToast()

const device = loadRemoteDesktopSession()?.device || t('remote.desktop.banner.unnamed')

watch(
  locale,
  async () => {
    const title = t('fileTransfer.windowTitle', { device })
    document.title = title
    try {
      await getCurrentWindow().setTitle(title)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

const localPane = ref<InstanceType<typeof FileTransferPane> | null>(null)
const remotePane = ref<InstanceType<typeof FileTransferPane> | null>(null)
const pane = (side: Side) => (side === 'local' ? localPane.value : remotePane.value)

// ── Queue ───────────────────────────────────────────────────────────────────

/** Destination panes to refresh once the files copied there have landed. */
const refreshAfter = new Set<Side>()
const queue = new TransferQueue({
  io: transferIo,
  // Items are mutated in place by the queue: make them reactive.
  wrap: <T extends object>(o: T) => reactive(o) as T,
  onSettled: (it) => {
    if (it.status === 'done') refreshAfter.add(destSide(it.direction))
    if (queue.items.some((i) => i.status === 'queued' || i.status === 'running')) return
    for (const side of refreshAfter) pane(side)?.refresh()
    refreshAfter.clear()
  },
})
const items = queue.items

// ── Conflicts ───────────────────────────────────────────────────────────────

const conflict = reactive({ open: false, name: '', isDir: false })
let answerConflict: ((a: ConflictAnswer | null) => void) | null = null

function askConflict(name: string, isDir: boolean): Promise<ConflictAnswer | null> {
  Object.assign(conflict, { open: true, name, isDir })
  return new Promise((resolve) => (answerConflict = resolve))
}

function onConflictAnswer(answer: ConflictAnswer | null) {
  conflict.open = false
  answerConflict?.(answer)
  answerConflict = null
}

// ── Starting a copy ─────────────────────────────────────────────────────────

const planning = ref(false)

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

async function transfer(direction: Direction, entries: FsEntry[], destDir: string | undefined) {
  if (planning.value || entries.length === 0 || !destDir) return
  planning.value = true
  const src = direction === 'up' ? localPaneFs : remotePaneFs
  const dest = paneFs(destSide(direction))
  try {
    const plan = await planTransfer(direction, entries, destDir, {
      walk: src.walk,
      list: async (dir) => (await dest.list(dir)).entries,
      mkdir: dest.mkdir,
      resolver: new ConflictResolver(askConflict),
    })
    for (const f of plan.failed) {
      const error = f.error === 'too_many_entries' ? t('fileTransfer.errors.tooMany') : f.error
      toast.error(t('fileTransfer.errors.planFailed', { name: f.name, error }), 6000)
    }
    if (plan.canceled) return
    queue.add(plan.jobs)
    if (plan.jobs.length === 0 && plan.failed.length === 0) toast.info(t('fileTransfer.nothingToCopy'))
  } catch (e) {
    toast.error(message(e), 6000)
  } finally {
    planning.value = false
    // Folders made while planning show up before their files land.
    pane(destSide(direction))?.refresh()
  }
}

const localSelection = computed(() => localPane.value?.selectedEntries ?? [])
const remoteSelection = computed(() => remotePane.value?.selectedEntries ?? [])

function sendToRemote() {
  void transfer('up', localSelection.value, remotePane.value?.dir)
}

function getFromRemote() {
  void transfer('down', remoteSelection.value, localPane.value?.dir)
}

// ── Drag and drop ───────────────────────────────────────────────────────────

/** Rows being dragged to the other pane. */
const drag = ref<{ from: Side; entries: FsEntry[]; x: number; y: number } | null>(null)
/** Folder (and side) a drag or an OS drop would copy into. */
const dropTarget = ref<{ side: Side; dir: string } | null>(null)

function targetAt(x: number, y: number): { side: Side; dir: string } | null {
  const el = document.elementFromPoint(x, y)
  const paneEl = el?.closest<HTMLElement>('[data-transfer-side]')
  if (!el || !paneEl) return null
  const side = paneEl.dataset.transferSide as Side
  const dir = el.closest<HTMLElement>('[data-transfer-dir]')?.dataset.transferDir || pane(side)?.dir
  return dir ? { side, dir } : null
}

function onDragStart(from: Side, entries: FsEntry[], x: number, y: number) {
  drag.value = { from, entries, x, y }
  const move = (ev: PointerEvent) => {
    if (!drag.value) return
    drag.value.x = ev.clientX
    drag.value.y = ev.clientY
    const target = targetAt(ev.clientX, ev.clientY)
    dropTarget.value = target && target.side !== from ? target : null
  }
  const end = (ev: PointerEvent | KeyboardEvent) => {
    if (ev instanceof KeyboardEvent && ev.key !== 'Escape') return
    window.removeEventListener('pointermove', move)
    window.removeEventListener('pointerup', end)
    window.removeEventListener('keydown', end)
    const target = ev instanceof PointerEvent ? dropTarget.value : null
    const dragged = drag.value
    drag.value = null
    dropTarget.value = null
    if (target && dragged) void transfer(dragged.from === 'local' ? 'up' : 'down', dragged.entries, target.dir)
  }
  window.addEventListener('pointermove', move)
  window.addEventListener('pointerup', end)
  window.addEventListener('keydown', end)
}

const dragLabel = computed(() => {
  const d = drag.value
  if (!d) return ''
  return d.entries.length === 1 ? d.entries[0].name : t('fileTransfer.items', { count: d.entries.length })
})

// Files dropped from Finder (this machine) onto the right pane are sent there.
let unlistenDrop: UnlistenFn | null = null

onMounted(async () => {
  try {
    unlistenDrop = await getCurrentWebview().onDragDropEvent(async (event) => {
      const p = event.payload
      if (p.type === 'leave') {
        dropTarget.value = null
        return
      }
      const { x, y } = nativeDropPoint(p.position)
      const target = targetAt(x, y)
      const ok = target?.side === 'remote' ? target : null
      if (p.type !== 'drop') {
        dropTarget.value = ok
        return
      }
      dropTarget.value = null
      if (!ok) return
      try {
        const { entries, failed } = await droppedEntries(p.paths)
        for (const f of failed) toast.error(t('fileTransfer.errors.planFailed', { name: f.name, error: f.error ?? '' }), 6000)
        await transfer('up', entries, ok.dir)
      } catch (e) {
        toast.error(message(e), 6000)
      }
    })
  } catch {
    /* outside the Tauri shell */
  }
})

// Closing the window mid-copy: drop the partial files (best effort; the
// controlled side also discards writes left idle for 10 minutes).
function onUnload() {
  queue.cancelAll()
}
window.addEventListener('beforeunload', onUnload)

onBeforeUnmount(() => {
  unlistenDrop?.()
  window.removeEventListener('beforeunload', onUnload)
  queue.cancelAll()
})
</script>

<template>
  <div class="flex h-screen w-screen flex-col gap-2 overflow-hidden bg-background p-3 text-foreground">
    <header class="flex shrink-0 items-baseline gap-3 px-1">
      <h1 class="text-sm font-semibold">{{ t('fileTransfer.title') }}</h1>
      <p class="truncate text-xs text-muted-foreground">{{ t('fileTransfer.hint', { device }) }}</p>
      <Loader2 v-if="planning" class="h-3.5 w-3.5 shrink-0 animate-spin self-center text-muted-foreground" />
    </header>

    <div class="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] gap-2">
      <FileTransferPane
        ref="localPane"
        :fs="localPaneFs"
        :title="t('fileTransfer.thisComputer')"
        :drop-dir="dropTarget?.side === 'local' ? dropTarget.dir : null"
        @drag-start="(entries, x, y) => onDragStart('local', entries, x, y)"
      />
      <div class="flex max-w-[11rem] flex-col items-stretch justify-center gap-2">
        <Button
          size="sm"
          class="w-full truncate"
          :disabled="planning || localSelection.length === 0"
          :title="t('fileTransfer.sendHint', { device })"
          @click="sendToRemote"
        >
          {{ t('fileTransfer.send', { device }) }} <ArrowRight class="h-3.5 w-3.5" />
        </Button>
        <Button
          variant="outline"
          size="sm"
          class="w-full truncate"
          :disabled="planning || remoteSelection.length === 0"
          :title="t('fileTransfer.getHint', { device })"
          @click="getFromRemote"
        >
          <ArrowLeft class="h-3.5 w-3.5" /> {{ t('fileTransfer.get', { device }) }}
        </Button>
      </div>
      <FileTransferPane
        ref="remotePane"
        :fs="remotePaneFs"
        :title="t('fileTransfer.controlled', { device })"
        :drop-dir="dropTarget?.side === 'remote' ? dropTarget.dir : null"
        @drag-start="(entries, x, y) => onDragStart('remote', entries, x, y)"
      />
    </div>

    <FileTransferQueue
      class="h-56 shrink-0"
      :items="items"
      @cancel="queue.cancel($event)"
      @retry="queue.retry($event)"
      @cancel-all="queue.cancelAll()"
      @retry-failed="queue.retryFailed()"
      @clear="queue.clearFinished()"
    />

    <div
      v-if="drag"
      class="pointer-events-none fixed z-[60] rounded-md border border-border bg-popover px-2 py-1 text-xs shadow-lg"
      :style="{ left: `${drag.x + 12}px`, top: `${drag.y + 12}px` }"
    >
      {{ dragLabel }}
    </div>

    <FileTransferConflict :open="conflict.open" :name="conflict.name" :is-dir="conflict.isDir" @answer="onConflictAnswer" />
    <ConfirmModal />
    <PromptModal />
    <ToastHost />
  </div>
</template>
