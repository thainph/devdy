import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  DISABLED_STATUS,
  deviceLabel,
  folderProjects,
  isFolderFinal,
  onSyncFolder,
  onSyncRun,
  onSyncStatus,
  overallPercent,
  parseLastRun,
  pickSource,
  sortDevices,
  syncBootstrapPending,
  syncDevicesList,
  syncFolderApply,
  syncFolderCancel,
  syncFolderPreview,
  syncPeersOnline,
  syncPrefsGet,
  syncPrefsSetSource,
  syncRun,
  syncStatus,
  defaultSelection,
  type FolderPreview,
  type FolderProgress,
  type LastSyncRun,
  type SyncDevice,
  type SyncPrefs,
  type SyncRunPhase,
  type SyncRunReport,
  type SyncStatus,
} from '@/lib/sync'

const LAST_RUN_KEY = 'devdy.sync.lastRun'
const INCLUDE_FOLDERS_KEY = 'devdy.sync.includeFolders'

/**
 * Where a manual Sync is: `data` = `sync_run` (pull + apply DB ops), `preview_loading` / `preview` =
 * the folder preview, `applying` = folder sync running in the background, `done` / `error` = result.
 */
export type RunStage = 'idle' | 'data' | 'preview_loading' | 'preview' | 'applying' | 'done' | 'error'

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function writeStorage(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch {
    // Storage unavailable: keep it in memory only.
  }
}

/**
 * Sync state shared by Settings → Sync, the Sync dialog and the sidebar indicator. The status is
 * kept fresh by `sync://status`; `refresh()` re-reads it after a command.
 * `mergePending`: joined a group but the first-join merge is not committed, so the
 * engine is stopped (status reads "disabled") until Settings → Sync commits it.
 * The on-demand run (`run`) lives here so the dialog can be closed ("run in background") and
 * reopened from the indicator.
 */
export const useSyncStore = defineStore('sync', () => {
  const status = ref<SyncStatus>({ ...DISABLED_STATUS })
  const mergePending = ref(false)
  const devices = ref<SyncDevice[]>([])
  const prefs = ref<SyncPrefs | null>(null)
  /** Other devices with a live relay connection; null = unknown (not asked yet / failed). */
  const peersOnline = ref<string[] | null>(null)
  const includeFolders = ref(readStorage(INCLUDE_FOLDERS_KEY) === '1')
  const lastRun = ref<LastSyncRun | null>(parseLastRun(readStorage(LAST_RUN_KEY)))
  const dialogOpen = ref(false)

  const run = reactive({
    stage: 'idle' as RunStage,
    source: '',
    includeFolders: false,
    phase: null as SyncRunPhase | null,
    phaseMessage: null as string | null,
    report: null as SyncRunReport | null,
    error: null as string | null,
    previews: [] as FolderPreview[],
    selected: [] as string[],
    /** Projects sent to `sync_folder_apply`. */
    applying: [] as string[],
    progress: {} as Record<string, FolderProgress>,
    /** First byte-moving update per project, for the ETA. */
    started: {} as Record<string, { at: number; bytes: number }>,
    stopped: false,
  })

  const unlisteners: UnlistenFn[] = []
  let starting: Promise<void> | null = null

  const source = computed(() => pickSource(prefs.value?.source_device, devices.value, peersOnline.value))
  const busy = computed(() => ['data', 'preview_loading', 'applying'].includes(run.stage))
  const percent = computed(() =>
    overallPercent(run.applying.map((id) => run.progress[id] ?? { phase: 'manifest', done_bytes: 0, total_bytes: 0 })),
  )

  function deviceName(id: string | null | undefined): string {
    if (!id) return ''
    const d = devices.value.find((x) => x.id === id)
    return d ? deviceLabel(d) : id.slice(0, 8)
  }

  /** Online per the last check; true while unknown so a failed check never blocks Sync. */
  function isOnline(id: string | null | undefined): boolean {
    if (!id) return false
    return peersOnline.value === null || peersOnline.value.includes(id)
  }

  async function refresh() {
    try {
      const [s, pending] = await Promise.all([syncStatus(), syncBootstrapPending()])
      status.value = s
      mergePending.value = pending
    } catch {
      // A build without sync commands: stay "disabled".
    }
  }

  function set(s: SyncStatus) {
    status.value = s
  }

  function setMergePending(v: boolean) {
    mergePending.value = v
  }

  /** Throws so Settings can report it. */
  async function loadDevices() {
    devices.value = sortDevices(await syncDevicesList())
  }

  async function loadPrefs() {
    prefs.value = await syncPrefsGet()
  }

  async function refreshPeers() {
    peersOnline.value = await syncPeersOnline().catch(() => null)
  }

  /** Load what the Sync button needs (devices, prefs, who is online); errors are swallowed. */
  async function loadForSync() {
    await Promise.all([loadDevices().catch(() => {}), loadPrefs().catch(() => {}), refreshPeers()])
  }

  async function setSource(id: string) {
    await syncPrefsSetSource(id)
    if (prefs.value) prefs.value = { ...prefs.value, source_device: id }
  }

  function setIncludeFolders(v: boolean) {
    includeFolders.value = v
    writeStorage(INCLUDE_FOLDERS_KEY, v ? '1' : '0')
  }

  function saveLastRun(r: LastSyncRun) {
    lastRun.value = r
    writeStorage(LAST_RUN_KEY, JSON.stringify(r))
  }

  function resetRun() {
    Object.assign(run, {
      stage: 'idle',
      source: '',
      includeFolders: false,
      phase: null,
      phaseMessage: null,
      report: null,
      error: null,
      previews: [],
      selected: [],
      applying: [],
      progress: {},
      started: {},
      stopped: false,
    })
  }

  /**
   * Sync from `src` (default: the chosen source): pull + apply its data, then — with folders —
   * load the folder preview and wait for the user. Opens the dialog. No-op while one runs.
   */
  async function syncFrom(src?: string | null, withFolders?: boolean) {
    if (busy.value) {
      dialogOpen.value = true
      return
    }
    const from = src ?? source.value
    if (!from) return
    resetRun()
    run.source = from
    run.includeFolders = withFolders ?? includeFolders.value
    run.stage = 'data'
    run.phase = 'requesting'
    dialogOpen.value = true
    try {
      run.report = await syncRun(from)
      run.phase = 'done'
      saveLastRun({
        at: Date.now(),
        source_device: from,
        ok: true,
        error: null,
        applied: run.report.applied,
        projects_ok: 0,
        projects_total: 0,
        project_errors: [],
      })
    } catch (e) {
      run.stage = 'error'
      run.phase = 'error'
      run.error = String(e)
      saveLastRun({
        at: Date.now(),
        source_device: from,
        ok: false,
        error: String(e),
        applied: 0,
        projects_ok: 0,
        projects_total: 0,
        project_errors: [],
      })
      void refreshPeers()
      return
    } finally {
      void refresh()
    }
    if (run.includeFolders) await loadPreview()
    else run.stage = 'done'
  }

  async function loadPreview() {
    await loadPrefs().catch(() => {})
    const ids = folderProjects(prefs.value?.projects ?? []).map((p) => p.project_id)
    if (!ids.length) {
      run.stage = 'done'
      return
    }
    run.stage = 'preview_loading'
    try {
      run.previews = await syncFolderPreview(run.source, ids)
      run.selected = defaultSelection(run.previews)
      run.stage = 'preview'
    } catch (e) {
      run.stage = 'error'
      run.error = String(e)
    }
  }

  /** The source's folder changed past the confirmed preview: drop the apply and preview again. */
  async function reviewAgain() {
    if (busy.value) return
    Object.assign(run, { applying: [], progress: {}, started: {}, stopped: false, error: null })
    await loadPreview()
  }

  function toggleSelected(id: string, on: boolean) {
    run.selected = on ? [...new Set([...run.selected, id])] : run.selected.filter((x) => x !== id)
  }

  /** Start folder sync for `ids` (default: the checked previews); progress arrives as events. */
  async function applyFolders(ids: string[] = run.selected) {
    if (!ids.length || run.stage === 'applying') return
    run.applying = [...ids]
    for (const id of ids) delete run.progress[id]
    for (const id of ids) delete run.started[id]
    run.stopped = false
    run.error = null
    run.stage = 'applying'
    try {
      await syncFolderApply(run.source, ids)
    } catch (e) {
      run.stage = 'error'
      run.error = String(e)
    }
  }

  async function cancelFolders() {
    if (run.stage !== 'applying') return
    run.stopped = true
    await syncFolderCancel().catch(() => {})
    finishFolders()
  }

  /** Failed projects of the last apply, for "Retry". */
  const failedIds = computed(() => run.applying.filter((id) => run.progress[id]?.phase === 'error'))

  function finishFolders() {
    run.stage = 'done'
    const names = new Map(run.previews.map((p) => [p.project_id, p.name]))
    const errors = failedIds.value.map((id) => ({
      project_id: id,
      name: names.get(id) ?? id,
      message: run.progress[id]?.message ?? '',
    }))
    const ok = run.applying.filter((id) => run.progress[id]?.phase === 'done').length
    const prev = lastRun.value
    saveLastRun({
      at: Date.now(),
      source_device: run.source,
      ok: !errors.length && !run.stopped && (prev?.ok ?? true),
      error: prev?.error ?? null,
      applied: run.report?.applied ?? prev?.applied ?? 0,
      projects_ok: ok,
      projects_total: run.applying.length,
      project_errors: errors,
    })
    void loadPrefs().catch(() => {})
  }

  function onFolder(e: FolderProgress) {
    if (!run.applying.includes(e.project_id)) return
    run.progress[e.project_id] = e
    if (!run.started[e.project_id] && e.phase === 'files') {
      run.started[e.project_id] = { at: Date.now(), bytes: e.done_bytes }
    }
    if (run.stage === 'applying' && run.applying.every((id) => isFolderFinal(run.progress[id]))) finishFolders()
  }

  /** Subscribe once (idempotent) and load the current status. */
  function start(): Promise<void> {
    starting ??= (async () => {
      const subs = await Promise.all([
        onSyncStatus(set).catch(() => null),
        onSyncRun((e) => {
          if (run.stage !== 'data') return
          run.phase = e.phase
          run.phaseMessage = e.message ?? null
        }).catch(() => null),
        onSyncFolder(onFolder).catch(() => null),
      ])
      for (const u of subs) if (u) unlisteners.push(u)
      await refresh()
    })()
    return starting
  }

  function stop() {
    for (const u of unlisteners.splice(0)) u()
    starting = null
  }

  return {
    status, mergePending, devices, prefs, peersOnline, includeFolders, lastRun, dialogOpen, run,
    source, busy, percent, failedIds,
    deviceName, isOnline, refresh, set, setMergePending, loadDevices, loadPrefs, refreshPeers, loadForSync,
    setSource, setIncludeFolders, syncFrom, loadPreview, reviewAgain, toggleSelected, applyFolders, cancelFolders, resetRun,
    start, stop,
  }
})
