// Opens THE Slack threads window (views/SlackThreadsWindow.vue,
// `?slackWindow=1[&id=][&add=1][&projectId=]`): a Slack-like two-pane reader of
// the threads Devdy fetched and translated. There is only ever ONE such window
// (fixed label); opening it again focuses it and re-points it at the requested
// thread / opens the add dialog.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'

/** Window label; src-tauri/capabilities/default.json grants it the app permissions. */
export const SLACK_WINDOW_LABEL = 'slack-threads'

/** Sent to the already-open window: select this thread. */
export const SLACK_WINDOW_SELECT = 'slack-window:select'
/** Sent to the already-open window: open the "+ Add thread" dialog. */
export const SLACK_WINDOW_ADD = 'slack-window:add'

/**
 * Emitted by the Slack window; the main window focuses itself and navigates
 * (the pop-out has no router of the app) — e.g. to the MCP settings when there
 * is no Slack token. Handled in App.vue.
 */
export const SLACK_WINDOW_OPEN_ROUTE = 'slack-window:open-route'

/**
 * Emitted by the backend when the user clicks the OS notification of a Slack
 * thread job started over the Inbox API (it also focuses the main window).
 * Only the main window listens (App.vue), so the Slack window opens once.
 */
export const SLACK_NOTIFICATION_CLICKED = 'capture-notification-clicked'

/**
 * Emitted by the backend when a Slack thread arrives over the Inbox API
 * (created or re-sent). Only the main window listens (App.vue) and opens the
 * Slack threads window on it.
 */
export const SLACK_API_RECEIVED = 'slack-threads://api-received'

export interface SlackApiReceived {
  id: string
}

export interface SlackNotificationClicked {
  id: string
  kind: 'slack'
}

export interface SlackWindowOpenRoute {
  path: string
}

export interface SlackWindowSelect {
  id: string
}

export interface SlackWindowAdd {
  projectId: string | null
}

export interface OpenSlackWindowOptions {
  /** Thread to select. */
  id?: string
  /** Open the add dialog. */
  add?: boolean
  /** Project to pre-filter the sidebar with (and default the add dialog to). */
  projectId?: string | null
}

export async function openSlackWindow(
  opts: OpenSlackWindowOptions = {},
): Promise<WebviewWindow | null> {
  const projectId = opts.projectId ?? null

  const existing = await WebviewWindow.getByLabel(SLACK_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      if (opts.id) await emit(SLACK_WINDOW_SELECT, { id: opts.id } satisfies SlackWindowSelect)
      if (opts.add) await emit(SLACK_WINDOW_ADD, { projectId } satisfies SlackWindowAdd)
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const params = new URLSearchParams({ slackWindow: '1' })
  if (opts.id) params.set('id', opts.id)
  if (opts.add) params.set('add', '1')
  if (projectId) params.set('projectId', projectId)

  const width = 1100
  const height = 760
  const win = new WebviewWindow(SLACK_WINDOW_LABEL, {
    url: `index.html?${params.toString()}`,
    title: 'Slack threads — Devdy',
    width,
    height,
    minWidth: 820,
    minHeight: 520,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[slackWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
