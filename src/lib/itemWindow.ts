// "New todo / note": opens the Todos / Notes screen (views/ItemListWindow.vue,
// a tab or its own window per the "Windows & tabs" setting) and asks it to show
// the capture form in its detail pane, where the item then opens once saved.
//
// The request travels as an app-wide event, because the screen may live in
// another webview (its own window) and may not exist yet. A freshly mounted list
// announces itself (ITEM_LIST_READY) and the opener re-sends the request then;
// Tauri events are not buffered. Requests carry an id, so a list that hears one
// twice acts once, and the placement the screen opened in, so only that copy of
// it (tab or window) answers.
import { emit, listen } from '@tauri-apps/api/event'
import type { Placement } from '@/lib/surfaces'

export type ItemKind = 'todo' | 'note'

/** Project / run the capture should be filed under, when opened from one. */
export interface ItemContext {
  projectId?: string | null
  runId?: string | null
}

/** Text to start a capture with (e.g. the selected run output). */
export interface ItemPrefill {
  title?: string
  content?: string
}

/** "Show the new-item form": sent to the Todos / Notes screen of `kind`. */
export interface ItemCreateRequest {
  id: string
  kind: ItemKind
  placement: Placement
  projectId: string | null
  runId: string | null
  prefill?: ItemPrefill
}

/** A Todos / Notes screen just mounted and is listening for requests. */
export interface ItemListReady {
  kind: ItemKind
  placement: Placement
}

export const ITEM_CREATE_REQUEST = 'item:create-request'
export const ITEM_LIST_READY = 'item:list-ready'

/**
 * Broadcast after any write to a todo / note (create, save, delete, toggle,
 * session links — the backend emits it too), so every window showing them — a
 * different webview, with its own store — refetches instead of showing stale
 * rows. Payload: `{ kind }`; no kind = both.
 */
export const ITEM_CHANGED = 'item:changed'

// Generous: a new window has to boot the SPA before it can listen.
const READY_TIMEOUT_MS = 6000

function hasPrefill(prefill?: ItemPrefill): boolean {
  return !!(prefill?.title?.trim() || prefill?.content?.trim())
}

/**
 * Open the Todos / Notes screen of `kind` on its new-item form. `prefill` only
 * fills fields that are still empty there, so it cannot clobber a capture the
 * user is already typing.
 */
export async function openItemCreate(
  kind: ItemKind = 'todo',
  context: ItemContext = {},
  prefill?: ItemPrefill,
): Promise<void> {
  const request: ItemCreateRequest = {
    id: crypto.randomUUID(),
    kind,
    placement: 'tab',
    projectId: context.projectId ?? null,
    runId: context.runId ?? null,
    prefill: hasPrefill(prefill) ? prefill : undefined,
  }
  // Armed before opening, so a screen that mounts fast still gets the request.
  const unlisten = await listen<ItemListReady>(ITEM_LIST_READY, (e) => {
    if (e.payload?.kind === kind && e.payload.placement === request.placement) {
      void emit(ITEM_CREATE_REQUEST, request).catch(() => {})
    }
  })
  setTimeout(unlisten, READY_TIMEOUT_MS)
  // Loaded on use: stores import ITEM_CHANGED from here, and surfaceOpen imports
  // the stores — a static import would make a cycle.
  const { openSurface } = await import('@/lib/surfaceOpen')
  request.placement = await openSurface(kind === 'todo' ? 'todos' : 'notes')
  // Already mounted: it hears this one.
  await emit(ITEM_CREATE_REQUEST, request).catch(() => {})
}
