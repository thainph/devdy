// The hideable list pane of a list-detail screen (Skills, Rules, MCP, Servers,
// Todos, Notes, Slack threads, a run's History).
//
// The screen calls `provideListPane(key)` once; the shared pieces read it with
// `useListPane()`: the sidebar hides itself (or floats over the detail while
// "peeking"), its header gets a hide button, and the detail header / empty
// state get show + peek buttons (components/listDetail/ListPaneButtons) while
// it is hidden. Hidden = the user hid it for this screen (remembered per
// screen), Focus mode (⌘.) is on, or — for a screen that opts in with
// `autoHide` — the screen is narrower than LIST_PANE_NARROW_BELOW (a split
// pane, a narrow window). Hidden for width is never written to the user's
// remembered choice: widen the screen and the list comes back as it was.
//
// ⌘B (`toggleActiveListPane`) acts on the pane of the screen in view: every
// provided pane registers itself with its root element, and the visible one
// (in the focused half of a split, if two are) wins.
import {
  computed,
  getCurrentInstance,
  inject,
  onBeforeUnmount,
  onMounted,
  provide,
  ref,
  watch,
  type ComputedRef,
  type InjectionKey,
  type Ref,
} from 'vue'
import { useUILayoutStore } from '@/stores/uiLayout'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { listPaneToggleAction, narrowFromWidth, type ListPaneKey } from '@/lib/listPane'

export interface ListPane {
  key: ListPaneKey
  /** The pane is out of the layout (hidden by the user, or Focus mode). */
  hidden: ComputedRef<boolean>
  /** Hidden by Focus mode rather than by the user. */
  byFocus: ComputedRef<boolean>
  /** The screen is too narrow for a docked list (autoHide): it can only peek. */
  narrow: Readonly<Ref<boolean>>
  /** While hidden: the list floats over the detail for a quick pick. */
  peeking: Ref<boolean>
  hide: () => void
  show: () => void
  toggle: () => void
  togglePeek: () => void
  closePeek: () => void
}

const KEY: InjectionKey<ListPane> = Symbol('listPane')

interface Registered {
  pane: ListPane
  el: () => Element | null
}
const registry = new Set<Registered>()

function visible(el: Element | null): boolean {
  return !!el && el.isConnected && el.getClientRects().length > 0
}

/** A component's first root element (`$el` is a text anchor when the root is a fragment). */
function rootElement(node: Node | null | undefined): Element | null {
  for (let n = node ?? null; n; n = n.nextSibling) if (n instanceof Element) return n
  return null
}

/** The pane of the screen in view (in the focused half of a split), if any. */
function activePane(): ListPane | null {
  const shown = [...registry].filter((r) => visible(r.el()))
  if (!shown.length) return null
  // While split, the half that last took a click (a press on non-focusable
  // space moves no DOM focus, only `focusedPane`); else where focus is.
  const tabs = useWorkspaceTabsStore()
  const focusPane = tabs.isSplit
    ? document.querySelector(`[data-pane="${tabs.focusedPane}"]`)
    : document.activeElement?.closest('[data-pane]')
  return ((focusPane && shown.find((r) => focusPane.contains(r.el()))) || shown[0]).pane
}

/** ⌘B: hide / show the list pane of the screen in view. False when there is none. */
export function toggleActiveListPane(): boolean {
  const pane = activePane()
  pane?.toggle()
  return !!pane
}

/**
 * For the View menu's label: is the list of the screen in view hidden? null
 * when that screen has no list. Reads the DOM — call after it has updated.
 */
export function activeListPaneHidden(): boolean | null {
  const pane = activePane()
  return pane ? pane.hidden.value : null
}

export interface ListPaneOptions {
  /** Follow the screen's own width: narrow below LIST_PANE_NARROW_BELOW. */
  autoHide?: boolean
  /**
   * The screen decides itself when it is narrow (RunView's body width) and
   * floats its list its own way: while `narrow`, a peek calls `peekNarrow`
   * instead of floating the docked list. Either way ⌘B never touches the
   * user's remembered hide choice while narrow.
   */
  narrow?: Readonly<Ref<boolean>>
  peekNarrow?: () => void
}

export function provideListPane(key: ListPaneKey, opts: ListPaneOptions = {}): ListPane {
  const ui = useUILayoutStore()
  const peeking = ref(false)
  const ownNarrow = ref(false)
  const narrow: Readonly<Ref<boolean>> = opts.narrow ?? ownNarrow
  const byFocus = computed(() => ui.focusMode)
  const hidden = computed(() => ui.focusMode || ui.isListHidden(key) || narrow.value)

  const pane: ListPane = {
    key,
    hidden,
    byFocus,
    narrow,
    peeking,
    hide: () => ui.setListHidden(key, true),
    show: () => {
      // In Focus mode "show the list" means leaving Focus.
      if (ui.focusMode) ui.focusMode = false
      ui.setListHidden(key, false)
      // A narrow screen can only float its list.
      peeking.value = narrow.value
    },
    toggle: () => {
      const action = listPaneToggleAction({ hidden: hidden.value, narrow: narrow.value })
      if (action === 'peek') pane.togglePeek()
      else if (action === 'show') pane.show()
      else pane.hide()
    },
    togglePeek: () => {
      if (narrow.value && opts.peekNarrow) opts.peekNarrow()
      else peeking.value = hidden.value && !peeking.value
    },
    closePeek: () => {
      peeking.value = false
    },
  }
  // Showing the pane for real ends a peek.
  watch(hidden, (h) => {
    if (!h) peeking.value = false
  })

  provide(KEY, pane)

  const instance = getCurrentInstance()
  const el = () => rootElement(instance?.proxy?.$el as Node | null)
  const entry: Registered = { pane, el }
  onMounted(() => registry.add(entry))
  onBeforeUnmount(() => registry.delete(entry))

  // autoHide: follow the screen's own width (not the window's).
  if (opts.autoHide && !opts.narrow && typeof ResizeObserver !== 'undefined') {
    let observer: ResizeObserver | null = null
    onMounted(() => {
      const root = el()
      if (!root) return
      // Measured now too, so a narrow screen never flashes its docked list.
      ownNarrow.value = narrowFromWidth(root.getBoundingClientRect().width, ownNarrow.value)
      observer = new ResizeObserver((entries) => {
        const width = entries[entries.length - 1]?.contentRect.width ?? 0
        ownNarrow.value = narrowFromWidth(width, ownNarrow.value)
      })
      observer.observe(root)
    })
    onBeforeUnmount(() => observer?.disconnect())
  }

  return pane
}

/** The pane of the enclosing screen, or null outside one (pop-out windows of other kinds…). */
export function useListPane(): ListPane | null {
  return inject(KEY, null)
}

/**
 * For the list pane's own root element: whether it renders at all, and the
 * classes that float it over the detail while peeking. A peek closes on Esc
 * or a press outside the list (picking a row is up to the caller).
 */
export function useListPaneSidebar(el: Ref<HTMLElement | null>, own?: ListPane) {
  // `own`: the screen renders its list itself (inject can't see its own provide).
  const pane = own ?? useListPane()
  const shown = computed(() => !pane || !pane.hidden.value || pane.peeking.value)
  const floating = computed(() => !!pane && pane.hidden.value && pane.peeking.value)
  const floatClass = computed(() =>
    floating.value ? 'absolute inset-y-0 left-0 z-30 shadow-2xl shadow-black/20' : '',
  )
  /**
   * Width of a list-detail sidebar: docked, 40% of the screen between 220 and
   * 360px (360 on a wide screen); floating, up to 340px of a narrow screen.
   */
  const widthClass = computed(() => (floating.value ? 'w-[min(340px,88%)]' : 'w-[clamp(220px,40%,360px)]'))

  function onPointerDown(e: PointerEvent) {
    if (!floating.value) return
    const target = e.target as Node | null
    // A press in a menu / dialog opened from the list (teleported to <body>) stays.
    // So does the ☰ toggle itself (its click closes the peek).
    if (target instanceof Element && target.closest('[role="dialog"], [role="menu"], .ui-menu-item, [data-list-peek]')) {
      return
    }
    if (el.value && target && !el.value.contains(target)) pane?.closePeek()
  }
  function onKeyDown(e: KeyboardEvent) {
    if (floating.value && e.key === 'Escape') {
      e.stopPropagation()
      pane?.closePeek()
    }
  }
  watch(floating, (on) => {
    if (on) {
      document.addEventListener('pointerdown', onPointerDown, true)
      document.addEventListener('keydown', onKeyDown, true)
    } else {
      document.removeEventListener('pointerdown', onPointerDown, true)
      document.removeEventListener('keydown', onKeyDown, true)
    }
  })
  onBeforeUnmount(() => {
    document.removeEventListener('pointerdown', onPointerDown, true)
    document.removeEventListener('keydown', onKeyDown, true)
  })

  /** Call when a row is picked: a peek closes, a docked list stays. */
  function picked() {
    if (floating.value) pane?.closePeek()
  }

  return { pane, shown, floating, floatClass, widthClass, picked }
}
