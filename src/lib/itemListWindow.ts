// Opens the Todos window or the Notes window (views/ItemListWindow.vue,
// `?itemListWindow=1&kind=…[&projectId=][&id=]`) — two independent master–detail
// windows laid out like the Slack threads window: a full-height sidebar of
// items, the selected one read and edited inline on the right.
//
// There is at most ONE window per kind (fixed label per kind), so todos and notes
// can sit side by side. Opening a kind again focuses its existing window and
// re-points it (project scope and / or the item to select); the other kind's
// window is never touched. App.vue detects the `itemListWindow` flag and renders
// a bare ItemListWindow.
//
// Every "open this note / todo" in the app lands here with `id`.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'
import type { ItemKind } from '@/lib/itemWindow'
import { openSlackWindow } from '@/lib/slackWindow'
import { popoutLabel, popoutUrl } from '@/lib/popout'

/**
 * The list windows' kinds: todos and notes. Slack threads have their own window
 * (lib/slackWindow); `'slack'` here is only accepted to redirect there.
 */
export type ItemListKind = ItemKind | 'slack'

const LIST_WINDOW_WIDTH = 1000
const LIST_WINDOW_HEIGHT = 720
// The second kind's window opens this much down-right of centre, so both show.
const LIST_WINDOW_OFFSET = 36

const LIST_WINDOW_TITLES: Record<ItemKind, string> = {
  todo: 'Todos — Devdy',
  note: 'Notes — Devdy',
}

/** The fixed window label of a kind's list window. */
export function itemListWindowLabel(kind: ItemKind): string {
  return popoutLabel(`item-list-${kind}`)
}

/**
 * Sent to an already-open list window to re-point it (project scope and / or
 * the item to select). Scoped per kind, so re-opening the todo window never
 * reaches the note window and vice versa.
 */
export function itemListSelectEvent(kind: ItemKind): string {
  return `item-list:select:${kind}`
}

export interface ItemListSelect {
  /** undefined = keep the current project scope. */
  projectId?: string | null
  /** Item to select. */
  id?: string | null
}

export interface OpenItemListOptions {
  /** Project to pre-filter the sidebar with (and default "+ New" to). */
  projectId?: string | null
  /** Item to select. */
  id?: string | null
}

/**
 * Open (or focus) the window of `kind`, optionally scoped to a project and /
 * or with an item selected.
 */
export async function openItemListWindow(
  kind: ItemListKind = 'todo',
  opts: OpenItemListOptions = {},
): Promise<WebviewWindow | null> {
  const projectId = opts.projectId ?? null
  // Slack threads moved out of this list into their own window.
  if (kind === 'slack') return openSlackWindow({ projectId, id: opts.id ?? undefined })

  const label = itemListWindowLabel(kind)
  const existing = await WebviewWindow.getByLabel(label)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      // Re-point the already-open window. Selecting an item leaves the
      // project scope alone (the window widens its filter if it must).
      await emit(itemListSelectEvent(kind), {
        projectId: opts.id ? undefined : projectId,
        id: opts.id ?? null,
      } satisfies ItemListSelect)
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const centre = await centerOnCurrentMonitor(LIST_WINDOW_WIDTH, LIST_WINDOW_HEIGHT)
  // The other kind's window already sits in the middle → step aside.
  const sibling = await WebviewWindow.getByLabel(
    itemListWindowLabel(kind === 'todo' ? 'note' : 'todo'),
  )
  if (centre && sibling) {
    centre.x += LIST_WINDOW_OFFSET
    centre.y += LIST_WINDOW_OFFSET
  }

  const params = new URLSearchParams({ itemListWindow: '1', kind })
  if (projectId) params.set('projectId', projectId)
  if (opts.id) params.set('id', opts.id)

  const win = new WebviewWindow(label, {
    url: popoutUrl(`index.html?${params.toString()}`),
    title: LIST_WINDOW_TITLES[kind],
    width: LIST_WINDOW_WIDTH,
    height: LIST_WINDOW_HEIGHT,
    minWidth: 760,
    minHeight: 480,
    ...centre,
  })
  win.once('tauri://error', (e) => {
    console.error('[itemListWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
