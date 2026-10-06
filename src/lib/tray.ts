// macOS menu-bar (system tray) window + session switcher.
//
// The tray icon is created in Rust (see src-tauri/src/commands/tray.rs); its menu
// is described here and pushed down via `set_tray_menu`, mirroring how the app
// menu bar works (appMenu.ts). That keeps labels on the app language and lets each
// session pop-out show its own run title.
//
// The menu lists every open window: the main window first, then each pop-out
// (session, gantt, file viewer, item editor). Clicking a row focuses that window;
// Rust maps the item id (the window label) back to the window. The mascot pet is
// skipped — it isn't a window you'd "switch to".
//
// Below the windows, the main window adds every running session (streaming or
// awaiting a permission). A conductor with active workers becomes a submenu so a
// big fan-out doesn't stretch the menu. Clicking a session is routed back to the
// main window (`tray://open-run`), which focuses the run's pop-out if it has one
// or shows the run in itself (see `openRunFromTray` callers in App.vue).
//
// The two sections are sent separately and Rust caches each (see tray.rs): any
// window may resend the window list, but only the main window knows every live
// run, so a pop-out's refresh must not touch the session section.
//
// Re-sent whenever the window set changes: on startup, when a session window opens
// or closes, and when a run is renamed (which updates the pop-out's OS title). The
// main window also re-sends whenever the session section's contents change.
import {
  getAllWebviewWindows,
  getCurrentWebviewWindow,
  type WebviewWindow,
} from '@tauri-apps/api/webviewWindow'
import { invoke } from '@/lib/tauri'
import { i18n } from '@/i18n'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useProjectsStore } from '@/stores/projects'

type TrayItem =
  | { kind: 'item'; id: string; label: string }
  | { kind: 'header'; label: string }
  | { kind: 'submenu'; label: string; items: TrayItem[] }
  | { kind: 'separator' }

/**
 * Keep the menu-bar switcher in sync as a pop-out comes and goes: call this right
 * after `new WebviewWindow(...)` so the window appears in the switcher once it's
 * created and drops out when it's closed. Any window the user can switch to should
 * be tracked (session, file viewer, gantt, item editor, …).
 */
export function trackWindowForTray(win: WebviewWindow): void {
  win.once('tauri://created', () => {
    void refreshTray()
  })
  win.once('tauri://destroyed', () => {
    void refreshTray()
  })
}

// Menu ids share a `tray:` namespace so the Rust app-menu handler ignores them
// (both handlers see every click — see tray.rs). `tray:win:<label>` focuses a
// window; `tray:run:<runId>` opens a session; Quit is appended by Rust.
const winId = (label: string) => `tray:win:${label}`
const runId = (id: string) => `tray:run:${id}`

/** Emitted by Rust to the main window when a session row is clicked. */
export const TRAY_OPEN_RUN = 'tray://open-run'
export interface TrayOpenRun {
  runId: string
}

function t(key: string, named?: Record<string, unknown>): string {
  return named ? i18n.global.t(key, named) : i18n.global.t(key)
}

// Windows that are never useful "switch to" targets: the always-on desktop pet
// and the transient permission modal (it pops in and auto-dismisses, so listing
// it would just make a row flicker). Everything else is listed.
const HIDDEN_WINDOW_LABELS = new Set(['mascot', 'permission-prompt'])

// The label shown for a window in the switcher. Some window kinds carry a
// genuinely meaningful OS title — a session (the run name), a file viewer (the
// file's basename), a todo/note editor ("Task: …" / "Note: …"), and the create
// and list windows ("New Task" / "Todos" …, following their active tab) — so
// those use the live title, falling back to a generic name. Every other kind gets a fixed, localized
// name keyed off its label prefix. Creation-time placeholders ("Devdy",
// "Task — Devdy") never count as meaningful: they only show until the window sets
// its real title.
function labelFor(windowLabel: string, osTitle: string): string {
  return clip(rawLabelFor(windowLabel, osTitle))
}

function rawLabelFor(windowLabel: string, osTitle: string): string {
  const title = osTitle.trim()
  const meaningful = title && title !== 'Devdy' && !title.endsWith('— Devdy') ? title : ''

  if (windowLabel === 'main') return t('tray.mainWindow')
  if (windowLabel.startsWith('session-')) return meaningful || t('tray.untitledSession')
  if (windowLabel.startsWith('fileviewer-')) return meaningful || t('tray.fileWindow')
  if (windowLabel === 'gantt') return t('tray.ganttWindow')
  if (windowLabel === 'item-create') return meaningful || t('tray.newItem')
  if (windowLabel.startsWith('item-edit-')) return meaningful || t('tray.editItem')
  if (windowLabel === 'item-list') return meaningful || t('tray.itemListWindow')
  // Unknown / future window kind: best effort — its title, else the raw label.
  return meaningful || windowLabel
}

// Native menu rows don't wrap; keep long run titles from blowing the menu wide.
const MAX_LABEL = 60
function clip(text: string): string {
  return text.length > MAX_LABEL ? `${text.slice(0, MAX_LABEL - 1)}…` : text
}

interface ActiveRun {
  runId: string
  projectId: string
  pending: number
}

// Same naming as the Active runs dock: issue/PR runs by number, sessions by title,
// else the project name — read from the session-wide `runMeta` cache so runs of
// background projects are named too. The project is appended for context.
function runLabel(r: ActiveRun): string {
  const { name, project } = runDisplayName(r.runId, r.projectId)
  return clip(project && name !== project ? `${name} — ${project}` : name)
}

/**
 * A run's human-readable name and its project's name (empty if unknown). Shared by
 * the tray and the permission windows so both identify a run the same way.
 */
export function runDisplayName(runId: string, projectId: string): { name: string; project: string } {
  const runs = useRunsStore()
  const project = useProjectsStore().projects.find((p) => p.id === projectId)?.name ?? ''
  const meta = runs.runMeta.get(runId)
  let name = ''
  if (meta && meta.run_type !== 'session' && meta.ref_number != null) {
    name = `${meta.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${meta.ref_number}`
  } else if (meta?.title?.trim()) {
    name = meta.title.trim()
  } else {
    name = project || t('tray.untitledSession')
  }
  return { name, project }
}

// ⚠ = blocked on a permission / question, ● = streaming.
const mark = (pending: number) => (pending > 0 ? '⚠ ' : '● ')

/**
 * The running-session section, built from the main window's stores (only it sees
 * every live run). Reads reactive state, so App.vue also uses it as a watch
 * source to resend the menu whenever the section's contents change.
 */
export function buildSessionItems(): TrayItem[] {
  const live = useLiveRunsStore()
  const runs = useRunsStore()

  const active: ActiveRun[] = []
  live.sessions.forEach((s) => {
    const pending = s.permissionQueue.length
    if (s.status === 'running' || pending > 0) {
      active.push({ runId: s.runId, projectId: s.projectId, pending })
    }
  })

  const header: TrayItem = {
    kind: 'header',
    label: active.length
      ? t('tray.runningSessions', { count: active.length })
      : t('tray.noRunningSessions'),
  }
  if (!active.length) return [header]

  // Workers are folded into a submenu under the conductor that spawned them.
  const workersByConductor = new Map<string, ActiveRun[]>()
  for (const r of active) {
    const parent = runs.runMeta.get(r.runId)?.conductor_run_id
    if (!parent) continue
    const list = workersByConductor.get(parent) ?? []
    list.push(r)
    workersByConductor.set(parent, list)
  }

  type Top = { pending: number; item: TrayItem }
  const tops: Top[] = []
  const seen = new Set<string>()
  const pushGroup = (conductor: ActiveRun, workers: ActiveRun[]) => {
    const pending = conductor.pending + workers.reduce((n, w) => n + w.pending, 0)
    workers.sort((a, b) => b.pending - a.pending)
    tops.push({
      pending,
      item: {
        kind: 'submenu',
        label: `${mark(pending)}${runLabel(conductor)} (${workers.length})`,
        items: [
          { kind: 'item', id: runId(conductor.runId), label: runLabel(conductor) },
          { kind: 'separator' },
          ...workers.map(
            (w): TrayItem => ({ kind: 'item', id: runId(w.runId), label: `${mark(w.pending)}${runLabel(w)}` }),
          ),
        ],
      },
    })
  }

  for (const r of active) {
    const parent = runs.runMeta.get(r.runId)?.conductor_run_id
    if (parent && workersByConductor.has(parent)) continue
    const workers = workersByConductor.get(r.runId)
    if (workers) {
      pushGroup(r, workers)
      seen.add(r.runId)
    } else {
      tops.push({
        pending: r.pending,
        item: { kind: 'item', id: runId(r.runId), label: `${mark(r.pending)}${runLabel(r)}` },
      })
    }
  }
  // A conductor can finish its own turn while its workers keep running; still
  // group those workers under it rather than letting them vanish.
  for (const [conductorId, workers] of workersByConductor) {
    if (seen.has(conductorId)) continue
    const meta = runs.runMeta.get(conductorId)
    pushGroup({ runId: conductorId, projectId: meta?.project_id ?? workers[0].projectId, pending: 0 }, workers)
  }

  // Runs awaiting the user first; otherwise keep a stable order.
  tops.sort((a, b) => Number(b.pending > 0) - Number(a.pending > 0))
  return [header, ...tops.map((x) => x.item)]
}

// Serialize refreshes: several triggers (open + title-set + destroyed) can fire in
// a burst, and set_tray_menu should apply them in order without overlapping.
let refreshing = false
let queued = false

/** Rebuild the tray menu from the currently open windows and push it to Rust. */
export async function refreshTray(): Promise<void> {
  if (refreshing) {
    queued = true
    return
  }
  refreshing = true
  try {
    const wins = await getAllWebviewWindows()
    const items: TrayItem[] = [{ kind: 'header', label: t('tray.windows') }]

    // Main window pinned first; every other Devdy window follows (except the
    // hidden ones above), sorted by label so the order stays stable across
    // refreshes rather than following getAllWebviewWindows' arbitrary order.
    if (wins.some((w) => w.label === 'main')) {
      items.push({ kind: 'item', id: winId('main'), label: t('tray.mainWindow') })
    }

    const others = wins
      .filter((w) => w.label !== 'main' && !HIDDEN_WINDOW_LABELS.has(w.label))
      .sort((a, b) => a.label.localeCompare(b.label))
    for (const w of others) {
      let title = ''
      try {
        title = await w.title()
      } catch {
        /* window may be mid-teardown */
      }
      items.push({ kind: 'item', id: winId(w.label), label: labelFor(w.label, title) })
    }

    // The badge = every running session (including ones never popped out into
    // their own window), so it must come from the main window's store — each
    // pop-out only tracks its own run. Same for the session section. Other
    // windows omit `title` / `sessions` entirely so they can't clobber what the
    // main window set (see tray.rs).
    const spec: {
      windows: TrayItem[]
      sessions?: TrayItem[]
      quitLabel: string
      title?: string
    } = { windows: items, quitLabel: t('tray.quit') }
    let isMain = false
    try {
      isMain = getCurrentWebviewWindow().label === 'main'
    } catch {
      /* outside the Tauri shell */
    }
    if (isMain) {
      const runningCount = useLiveRunsStore().runningIds.length
      spec.title = runningCount > 0 ? String(runningCount) : ''
      spec.sessions = buildSessionItems()
    }

    await invoke('set_tray_menu', { spec })
  } catch {
    // Outside the Tauri shell (browser dev) there is no tray.
  } finally {
    refreshing = false
    if (queued) {
      queued = false
      void refreshTray()
    }
  }
}
