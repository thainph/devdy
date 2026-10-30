<script setup lang="ts">
// The on-demand Sync dialog (app-wide, state in the sync store): pull + apply data from the
// source device (`sync_run`, phases from `sync://run`), then the folder preview of the "data +
// folder" projects (`sync_folder_preview`) — shown only when something in it is risky (risky
// projects expanded, safe ones collapsed), else applied straight away — then folder sync progress
// (`sync_folder_apply`, `sync://folder`) with "Run in background" (close, keep running) and "Stop"
// (`sync_folder_cancel`). Skipped projects are listed with the result.
// Opened from Settings → Sync and the sidebar SyncIndicator popover.
import { computed, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openPath } from '@tauri-apps/plugin-opener'
import {
  AlertTriangle, Check, ChevronDown, ChevronRight, FolderOpen, Loader2, RefreshCw, RotateCcw, Square, X,
} from 'lucide-vue-next'
import { Button, Modal } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useSyncStore } from '@/stores/sync'
import {
  backupDir,
  canApplyPreview,
  etaSeconds,
  folderPercent,
  formatBytes,
  formatEta,
  isPlanChanged,
  isSafePreview,
  isTruncated,
  previewIsNoop,
  previewTotals,
  runProgress,
  summarizePreview,
  syncErrorCode,
  type FolderPreview,
  type PreviewFile,
} from '@/lib/sync'

const { t, te } = useI18n()
const router = useRouter()
const { toast } = useToast()
const store = useSyncStore()
const run = store.run

const sourceName = computed(() => store.deviceName(run.source))
const progress = computed(() => runProgress(run.event))
const offline = computed(() => syncErrorCode(run.error) === 'peer_offline')

function errorText(e: unknown): string {
  const code = syncErrorCode(e)
  return code ? t(`settings.sync.errors.${code}`) : String(e)
}

const summaries = computed(() => new Map(run.previews.map((p) => [p.project_id, summarizePreview(p)])))
const totals = computed(() => previewTotals(run.previews, run.selected))
const expanded = ref<Set<string>>(new Set())

function toggleExpanded(id: string) {
  const next = new Set(expanded.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  expanded.value = next
}

/** Safe projects start collapsed in the preview; risky ones are always open. */
const openSafe = ref<Set<string>>(new Set())

function isOpen(p: FolderPreview): boolean {
  return !isSafePreview(p) || openSafe.value.has(p.project_id)
}

function toggleSafe(id: string) {
  const next = new Set(openSafe.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  openSafe.value = next
}

const skippedPreviews = computed(() => run.previews.filter((p) => p.skipped))

function skipLabel(reason: string): string {
  const key = `settings.sync.run.skipped.${reason}`
  return te(key) ? t(key) : t('settings.sync.run.skipped.other', { reason })
}

function skipItemLabel(reason: string): string {
  const key = `settings.sync.run.skipItem.${reason}`
  return te(key) ? t(key) : reason
}

/** The file lists of a preview with their exact counts (the lists themselves are capped). */
function fileLists(p: FolderPreview): Array<{ key: string; files: PreviewFile[]; count: number; truncated: boolean }> {
  return ([
    ['listOverwrite', p.overwrite, p.overwrite_count],
    ['listAdd', p.add, p.add_count],
    ['listDelete', p.delete, p.delete_count],
  ] as const)
    .map(([key, files, count]) => ({
      key,
      files: files ?? [],
      count: Math.max(count ?? 0, files?.length ?? 0),
      truncated: isTruncated(files, count),
    }))
    .filter((l) => l.count > 0)
}

function branchLabel(name: string | null): string {
  return name ?? t('settings.sync.run.preview.detached')
}

const previewById = computed(() => new Map(run.previews.map((p) => [p.project_id, p])))
function previewOf(id: string): FolderPreview | undefined {
  return previewById.value.get(id)
}

// ETA re-renders on this clock while folders sync.
const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null
watch(
  () => run.stage === 'applying' && store.dialogOpen,
  (on) => {
    if (on && !clock) clock = setInterval(() => (now.value = Date.now()), 1000)
    else if (!on && clock) {
      clearInterval(clock)
      clock = null
    }
  },
  { immediate: true },
)
onUnmounted(() => {
  if (clock) clearInterval(clock)
})

function eta(id: string): string | null {
  const p = run.progress[id]
  const s = run.started[id]
  if (!p || !s || p.phase !== 'files') return null
  return formatEta(etaSeconds(p.done_bytes, p.total_bytes, s.at, now.value, s.bytes))
}

const doneCount = computed(() => run.applying.filter((id) => run.progress[id]?.phase === 'done').length)

function close() {
  store.dialogOpen = false
  if (!store.busy) store.resetRun()
}

function runInBackground() {
  store.dialogOpen = false
}

function cancelPreview() {
  close()
}

/** Retry whatever failed: the data pull, the preview, starting the apply, or the failed projects. */
function retry() {
  if (!run.report) void store.syncFrom(run.source)
  else if (store.failedIds.length) void store.applyFolders(store.failedIds)
  else if (run.applying.length) void store.applyFolders(run.applying)
  else void store.loadPreview()
}

/** Apply aborted: the source's folder changed beyond the confirmed preview. */
const planChanged = computed(() =>
  isPlanChanged(run.error) || run.applying.some((id) => isPlanChanged(run.progress[id]?.message)))

const canRetry = computed(() =>
  !planChanged.value && (run.stage === 'error' || (run.stage === 'done' && store.failedIds.length > 0)))

function openProject(id: string) {
  close()
  router.push(`/projects/${id}`).catch(() => {})
}

async function viewBackup(id: string) {
  const path = previewOf(id)?.local_path
  if (!path) return
  await openPath(backupDir(path)).catch(() => toast.error(backupDir(path)))
}
</script>

<template>
  <Modal :open="store.dialogOpen" size="lg" scroll-body :dismiss-on-overlay="false" @close="close">
    <template #header>
      <RefreshCw class="h-3.5 w-3.5 text-muted-foreground" :class="{ 'animate-spin': store.busy }" :stroke-width="1.5" />
      <h3 class="flex-1 text-sm font-semibold">{{ t('settings.sync.run.title', { name: sourceName }) }}</h3>
    </template>

    <div class="space-y-4 p-4">
      <!-- Data (sync_run) -->
      <section class="space-y-1.5">
        <p v-if="run.stage === 'data'" class="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
          {{ t(`settings.sync.run.phase.${run.phase ?? 'requesting'}`, { name: sourceName }) }}
          <span v-if="progress?.kind === 'files'" class="tabular-nums">
            {{ t('settings.sync.run.files', { done: formatBytes(progress.done), total: formatBytes(progress.total), n: progress.filesDone, m: progress.filesTotal, p: progress.percent }) }}
          </span>
          <span v-else-if="progress?.kind === 'ops'" class="tabular-nums">{{ t('settings.sync.run.ops', { n: progress.ops }) }}</span>
        </p>
        <p v-else-if="run.report" class="text-xs text-emerald-600 dark:text-emerald-400">
          {{ t('settings.sync.run.dataDone', { n: run.report.applied }) }}
        </p>
        <div
          v-if="run.stage === 'error'"
          class="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-xs"
        >
          <AlertTriangle class="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="1.75" />
          <p class="min-w-0 flex-1 break-words">
            {{ offline ? t('settings.sync.ondemand.offlineHelp', { name: sourceName }) : errorText(run.error) }}
          </p>
        </div>
      </section>

      <!-- Folder preview loading -->
      <p v-if="run.stage === 'preview_loading'" class="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ t('settings.sync.run.previewLoading') }}
      </p>
      <p
        v-else-if="run.stage === 'done' && run.report && !run.previews.length"
        class="text-[11px] text-muted-foreground"
      >
        {{ t('settings.sync.run.noFolders') }}
      </p>

      <!-- Folder preview -->
      <section v-if="run.stage === 'preview'" class="space-y-2">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
          {{ t('settings.sync.run.previewTitle') }}
        </label>
        <ul class="divide-y divide-border/60 rounded-lg border border-border/60">
          <li v-for="p in run.previews" :key="p.project_id" class="space-y-1 px-3 py-2">
            <label class="flex items-center gap-2" :class="canApplyPreview(p) ? 'cursor-pointer' : 'opacity-60'">
              <input
                type="checkbox"
                class="accent-primary"
                :checked="canApplyPreview(p) && run.selected.includes(p.project_id)"
                :disabled="!canApplyPreview(p)"
                @change="store.toggleSelected(p.project_id, ($event.target as HTMLInputElement).checked)"
              >
              <span class="truncate text-sm">{{ p.name }}</span>
              <span v-if="p.local_path" class="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground" :title="p.local_path">
                {{ p.local_path }}
              </span>
              <span v-else class="flex-1" />
              <button
                v-if="isSafePreview(p)"
                type="button"
                class="flex shrink-0 items-center gap-0.5 text-[11px] text-muted-foreground hover:text-foreground"
                @click.prevent="toggleSafe(p.project_id)"
              >
                {{ t('settings.sync.run.preview.safe', { size: formatBytes(p.download_bytes) }) }}
                <component :is="isOpen(p) ? ChevronDown : ChevronRight" class="h-3 w-3" :stroke-width="1.75" />
              </button>
            </label>
            <div v-if="isOpen(p)" class="space-y-0.5 pl-6 text-[11px]">
              <template v-if="p.skipped">
                <p class="text-muted-foreground">{{ skipLabel(p.skipped) }}</p>
              </template>
              <p v-else-if="p.error" class="break-words text-destructive">{{ errorText(p.error) }}</p>
              <template v-else-if="summaries.get(p.project_id)">
                <template v-for="(s, si) in [summaries.get(p.project_id)!]" :key="si">
                  <p v-if="s.create" class="text-sky-600 dark:text-sky-400">
                    {{ t('settings.sync.run.preview.create', { n: s.clones, size: formatBytes(s.bytes) }) }}
                  </p>
                  <template v-else>
                    <p v-if="previewIsNoop(s)" class="text-muted-foreground">{{ t('settings.sync.run.preview.noop') }}</p>
                    <p v-for="b in s.branches" :key="b.repo" class="font-mono">
                      <span v-if="b.repo" class="text-muted-foreground">{{ b.repo }}: </span>
                      {{ t('settings.sync.run.preview.branch', { from: branchLabel(b.from), to: branchLabel(b.to), sha: b.sha }) }}
                    </p>
                    <p v-if="s.commits || s.staged" class="text-muted-foreground">
                      {{ [
                        s.commits ? t('settings.sync.run.preview.commits', { n: s.commits }) : '',
                        s.staged ? t('settings.sync.run.preview.staged', { n: s.staged }) : '',
                      ].filter(Boolean).join(' · ') }}
                    </p>
                    <p v-if="!previewIsNoop(s)" class="text-muted-foreground">
                      {{ t('settings.sync.run.preview.files', {
                        overwrite: s.overwrite, add: s.add, del: s.delete, size: formatBytes(s.bytes),
                      }) }}
                    </p>
                  </template>
                  <p v-if="s.localChanges" class="flex items-center gap-1 text-amber-600 dark:text-amber-400">
                    <AlertTriangle class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                    {{ t('settings.sync.run.preview.localChanges', { n: s.localChanges }) }}
                  </p>
                  <p v-if="s.tooLarge" class="text-amber-600 dark:text-amber-400">
                    {{ t('settings.sync.run.preview.tooLarge', { n: s.tooLarge }) }}
                  </p>
                  <template v-for="r in s.rewound" :key="`rw:${r.repo}`">
                    <p v-for="b in r.branches" :key="`${r.repo}:${b}`" class="flex items-start gap-1 text-amber-600 dark:text-amber-400">
                      <AlertTriangle class="mt-px h-3 w-3 shrink-0" :stroke-width="1.75" />
                      <span>
                        <span v-if="r.repo" class="font-mono">{{ r.repo }}: </span>
                        {{ t('settings.sync.run.preview.rewound', { branch: b }) }}
                      </span>
                    </p>
                  </template>
                  <details v-if="p.skipped_items?.length" class="text-muted-foreground">
                    <summary class="cursor-pointer">{{ t('settings.sync.run.preview.skippedItems', { n: p.skipped_items.length }) }}</summary>
                    <ul class="mt-1 max-h-32 space-y-0.5 overflow-y-auto pl-3">
                      <li v-for="it in p.skipped_items" :key="`${it.reason}:${it.path}`" class="flex justify-between gap-3">
                        <span class="truncate font-mono" :title="it.path">{{ it.path || '.' }}</span>
                        <span class="shrink-0">{{ skipItemLabel(it.reason) }}<template v-if="it.size"> · {{ formatBytes(it.size) }}</template></span>
                      </li>
                    </ul>
                  </details>
                  <template v-if="s.overwrite || s.add || s.delete || s.tooLarge">
                    <button
                      type="button"
                      class="flex items-center gap-0.5 text-muted-foreground hover:text-foreground"
                      @click="toggleExpanded(p.project_id)"
                    >
                      <component :is="expanded.has(p.project_id) ? ChevronDown : ChevronRight" class="h-3 w-3" :stroke-width="1.75" />
                      {{ expanded.has(p.project_id) ? t('settings.sync.run.preview.hideDetails') : t('settings.sync.run.preview.details') }}
                    </button>
                    <div v-if="expanded.has(p.project_id)" class="max-h-48 space-y-1.5 overflow-y-auto rounded-md border border-border/60 p-2">
                      <div v-for="l in fileLists(p)" :key="l.key">
                        <p class="font-medium text-muted-foreground">{{ t(`settings.sync.run.preview.${l.key}`) }} ({{ l.count }})</p>
                        <p v-if="l.truncated" class="text-amber-600 dark:text-amber-400">
                          {{ t('settings.sync.run.preview.truncated', { shown: l.files.length, total: l.count }) }}
                        </p>
                        <p v-for="f in l.files" :key="f.path" class="flex justify-between gap-3 font-mono">
                          <span class="truncate" :title="f.path">{{ f.path }}</span>
                          <span class="shrink-0 text-muted-foreground">{{ formatBytes(f.size) }}</span>
                        </p>
                      </div>
                      <div v-if="p.too_large?.length">
                        <p class="font-medium text-amber-600 dark:text-amber-400">{{ t('settings.sync.run.preview.tooLarge', { n: p.too_large.length }) }}</p>
                        <p v-for="f in p.too_large" :key="f.path" class="flex justify-between gap-3 font-mono">
                          <span class="truncate" :title="f.path">{{ f.path }}</span>
                          <span class="shrink-0 text-muted-foreground">{{ formatBytes(f.size) }}</span>
                        </p>
                      </div>
                    </div>
                  </template>
                </template>
              </template>
            </div>
          </li>
        </ul>
      </section>

      <!-- Apply refused: the source changed since the preview -->
      <div
        v-if="planChanged && !store.busy"
        class="flex items-start gap-2 rounded-lg border border-amber-500/50 bg-amber-500/5 p-3 text-xs"
      >
        <AlertTriangle class="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-500" :stroke-width="1.75" />
        <p class="min-w-0 flex-1">{{ t('settings.sync.run.planChanged') }}</p>
        <Button variant="outline" size="xs" @click="store.reviewAgain()">{{ t('settings.sync.run.review') }}</Button>
      </div>

      <!-- Skipped projects (never block applying) -->
      <section v-if="run.stage !== 'preview' && skippedPreviews.length" class="space-y-1 text-[11px] text-muted-foreground">
        <p class="font-medium">{{ t('settings.sync.run.skippedTitle', { n: skippedPreviews.length }) }}</p>
        <ul class="space-y-0.5">
          <li v-for="p in skippedPreviews" :key="p.project_id" class="flex min-w-0 gap-2">
            <span class="min-w-0 truncate text-foreground">{{ p.name }}</span>
            <span class="shrink-0">{{ skipLabel(p.skipped!) }}</span>
          </li>
        </ul>
      </section>

      <!-- Folder progress / result -->
      <section v-if="run.applying.length && (run.stage === 'applying' || run.stage === 'done' || run.stage === 'error')" class="space-y-2">
        <p v-if="run.stage === 'done' && doneCount" class="text-xs text-emerald-600 dark:text-emerald-400">
          {{ t('settings.sync.run.ready', { n: doneCount }) }}
        </p>
        <p v-if="run.stage === 'done' && run.stopped" class="text-[11px] text-muted-foreground">{{ t('settings.sync.run.stopped') }}</p>
        <ul class="divide-y divide-border/60 rounded-lg border border-border/60">
          <li v-for="id in run.applying" :key="id" class="space-y-1.5 px-3 py-2">
            <div class="flex items-center gap-2 text-sm">
              <Check v-if="run.progress[id]?.phase === 'done'" class="h-3.5 w-3.5 shrink-0 text-emerald-500" :stroke-width="2" />
              <X v-else-if="run.progress[id]?.phase === 'error'" class="h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="2" />
              <Loader2 v-else-if="run.stage === 'applying'" class="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" :stroke-width="1.75" />
              <Square v-else class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <span class="min-w-0 flex-1 truncate">{{ previewOf(id)?.name ?? id }}</span>
              <span class="shrink-0 text-[11px] text-muted-foreground">
                {{ run.progress[id] ? t(`settings.sync.run.folderPhase.${run.progress[id].phase}`) : t('settings.sync.run.waiting') }}
              </span>
            </div>
            <template v-if="run.progress[id]?.phase !== 'done' && run.progress[id]?.phase !== 'error' && run.progress[id]?.phase !== 'skipped'">
              <div class="h-1.5 overflow-hidden rounded-full bg-muted">
                <div class="h-full rounded-full bg-primary transition-[width]" :style="{ width: `${folderPercent(run.progress[id])}%` }" />
              </div>
              <p class="flex flex-wrap gap-x-2 text-[11px] tabular-nums text-muted-foreground">
                <span>{{ folderPercent(run.progress[id]) }}%</span>
                <span v-if="run.progress[id]?.total_bytes">
                  {{ formatBytes(run.progress[id].done_bytes) }} / {{ formatBytes(run.progress[id].total_bytes) }}
                </span>
                <span v-if="run.progress[id]?.files_total">
                  {{ t('settings.sync.run.files', { done: run.progress[id].files_done, total: run.progress[id].files_total }) }}
                </span>
                <span v-if="eta(id)">{{ t('settings.sync.run.eta', { eta: eta(id) }) }}</span>
              </p>
            </template>
            <p v-if="run.progress[id]?.phase === 'error' && run.progress[id]?.message" class="break-words text-[11px] text-destructive">
              {{ errorText(run.progress[id].message) }}
            </p>
            <div v-if="run.stage === 'done' && run.progress[id]?.phase === 'done'" class="flex flex-wrap gap-2">
              <Button variant="outline" size="xs" @click="openProject(id)">{{ t('settings.sync.run.openProject') }}</Button>
              <Button v-if="previewOf(id)?.local_path" variant="ghost" size="xs" @click="viewBackup(id)">
                <FolderOpen class="h-3 w-3" :stroke-width="1.75" />
                {{ t('settings.sync.run.viewBackup') }}
              </Button>
            </div>
          </li>
        </ul>
      </section>
    </div>

    <template #footer>
      <template v-if="run.stage === 'preview'">
        <p class="mr-auto text-[11px] text-muted-foreground">
          {{ t('settings.sync.run.totals', { size: formatBytes(totals.bytes) }) }}<template v-if="totals.backup"> · {{ t('settings.sync.run.totalsBackup') }}</template>
        </p>
        <Button variant="outline" size="sm" @click="cancelPreview">{{ t('settings.sync.run.cancel') }}</Button>
        <Button size="sm" :disabled="!totals.count" @click="store.applyFolders()">
          {{ totals.files ? t('settings.sync.run.applyFiles', { n: totals.count, files: totals.files }) : t('settings.sync.run.apply', { n: totals.count }) }}
        </Button>
      </template>
      <template v-else-if="run.stage === 'applying'">
        <Button variant="outline" size="sm" @click="store.cancelFolders()">
          <Square class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('settings.sync.run.stop') }}
        </Button>
        <Button size="sm" @click="runInBackground">{{ t('settings.sync.run.background') }}</Button>
      </template>
      <template v-else-if="run.stage === 'data' || run.stage === 'preview_loading'">
        <Button variant="outline" size="sm" @click="runInBackground">{{ t('settings.sync.run.background') }}</Button>
      </template>
      <template v-else>
        <Button v-if="canRetry" variant="outline" size="sm" @click="retry">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('settings.sync.run.retry') }}
        </Button>
        <Button size="sm" @click="close">{{ t('settings.sync.run.close') }}</Button>
      </template>
    </template>
  </Modal>
</template>
