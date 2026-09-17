<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { RouterLink, RouterView, useRoute, useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useProjectsStore } from '@/stores/projects'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useUILayoutStore } from '@/stores/uiLayout'
import { useI18n } from 'vue-i18n'
import { setLocale } from '@/i18n'
import { ListTodo, PanelLeftClose, PanelLeftOpen } from 'lucide-vue-next'
import PermissionNotifier from '@/components/PermissionNotifier.vue'
import CalendarReminder from '@/components/CalendarReminder.vue'
import WorkspaceTabs from '@/components/WorkspaceTabs.vue'
import ActiveRunsDock from '@/components/ActiveRunsDock.vue'
import ActiveRemoteDock from '@/components/ActiveRemoteDock.vue'
import FileViewerWindow from '@/views/FileViewerWindow.vue'
import PermissionWindow from '@/views/PermissionWindow.vue'
import ThemeDecorations from '@/components/ThemeDecorations.vue'
import { Button, ConfirmModal, PromptModal, ToastHost } from '@/components/ui'
import ImageCompareHost from '@/components/ImageCompareHost.vue'
import CyberFoxHost from '@/components/CyberFoxHost.vue'
import MascotWindow from '@/views/MascotWindow.vue'
import ItemWindow from '@/views/ItemWindow.vue'
import ItemListDrawer from '@/components/ItemListDrawer.vue'
import { useItemPanel } from '@/composables/useItemPanel'
import { openItemCreateWindow } from '@/lib/itemWindow'
import { applyAppMenu, IS_MAC, listenMenuActions, registerMenuAction, runMenuAction } from '@/lib/appMenu'
import { NAV_ROUTES } from '@/lib/navigation'
import IssuesGanttView from '@/views/IssuesGanttView.vue'
import { getVersion } from '@tauri-apps/api/app'

// Pop-out windows load the same SPA with a query flag; render a bare,
// chrome-less host (no sidebar / nav / background work) in those cases.
const isFileWindow = new URLSearchParams(window.location.search).get('fileWindow') === '1'
const isPermissionWindow = new URLSearchParams(window.location.search).get('permissionWindow') === '1'
const isItemWindow = new URLSearchParams(window.location.search).get('itemWindow') === '1'
const isGanttWindow = new URLSearchParams(window.location.search).get('ganttWindow') === '1'
const isMascotWindow = new URLSearchParams(window.location.search).get('mascotWindow') === '1'
// All pop-out kinds only need the theme applied; skip the main app's data work.
const isPopoutWindow =
  isFileWindow || isPermissionWindow || isItemWindow || isGanttWindow || isMascotWindow

const route = useRoute()
const router = useRouter()
const projectsStore = useProjectsStore()
const appSettings = useAppSettingsStore()
const tabsStore = useWorkspaceTabsStore()
const uiLayout = useUILayoutStore()
const live = useLiveRunsStore()
const { t, locale } = useI18n()
const itemPanel = useItemPanel()

const isRunRoute = computed(
  () => route.name === 'project-run' || route.name === 'project-run-detail',
)

// Color themes that ship an animated decorative background scene. When one is
// active AND the user hasn't turned animations off, the app root is made
// transparent so the body-level scene (ThemeDecorations) shows through.
const SCENIC_THEMES = new Set(['midautumn'])
const sceneTheme = computed(() => appSettings.settings?.color_theme ?? 'default')
const sceneOn = computed(
  () =>
    SCENIC_THEMES.has(sceneTheme.value) &&
    appSettings.settings?.animated_background !== 'false',
)

// Force a clean RunView remount only when the *project* changes (heavy stream
// state survives in the liveRuns store); switching runs within a project is
// handled in-place by RunView's own activeRunId watcher. Non-run routes keep a
// stable per-view key so their existing reuse behaviour is unchanged.
const routeKey = computed(() =>
  isRunRoute.value ? `run-${route.params.projectId}` : String(route.name ?? route.path),
)

// Views kept alive across navigation, so returning to one is instant: no
// teardown, no setup() re-run, no DOM rebuild, and scroll position + form state
// survive. Each still refreshes its data on re-entry via `useActivatedRefresh`.
//
// Matched by COMPONENT NAME, which is why every view listed here declares
// `defineOptions({ name })` rather than relying on the name Vue infers from the
// filename.
//
// RunView is deliberately absent: its heavy stream state already lives in the
// liveRuns store (so it survives navigation anyway) and `routeKey` exists
// precisely to force it to remount when the project changes. Do not add it.
//
// Only views with no timers, no event listeners and no cleanup hooks are listed.
// Anything with a lifecycle side effect needs its `onMounted`/`onUnmounted` work
// moved to `onActivated`/`onDeactivated` first — under KeepAlive those hooks no
// longer fire on navigation, so a timer would otherwise run forever in the
// background.
const CACHED_VIEWS = [
  'SkillsView',
  'RulesView',
  'McpServersView',
  'CalendarView',
  'WorkDigestView',
  'PrInboxView',
  'ServersView',
]

// Pin every project the user opens as a workspace tab; remember the run being
// viewed so re-selecting the tab returns to it.
watch(
  () => [route.name, route.params.projectId, route.params.runId] as const,
  ([name, projectId, runId]) => {
    if ((name === 'project-run' || name === 'project-run-detail') && typeof projectId === 'string') {
      tabsStore.open(projectId, typeof runId === 'string' ? runId : null)
    }
  },
  { immediate: true },
)

// Global listener that mirrors backend-reported active runs into the liveRuns
// store (see onMounted). Torn down on unmount to avoid a dangling subscription.
let unlistenActivated: UnlistenFn | null = null

// True when the event target is a text-entry surface where Backspace/navigation
// keys are legitimately used to edit text.
function isEditableTarget(el: EventTarget | null): boolean {
  const node = el as HTMLElement | null
  if (!node || typeof node.closest !== 'function') return false
  if (node.isContentEditable) return true
  return !!node.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]')
}

// Swallow navigation keys (mainly Backspace) when focus is NOT in a text field.
// Vietnamese IMEs (Telex/VNI) compose diacritics by injecting Backspace at the
// OS level; if the composer hasn't been focused yet, those Backspaces reach the
// webview and trigger history "back" navigation — the app appears to jump to a
// different screen while the user is just typing. Blocking them here is the
// backstop; RunView still focuses the composer on session open.
function onGlobalNavKeyGuard(e: KeyboardEvent) {
  if (e.key === 'Backspace' && !isEditableTarget(e.target)) {
    e.preventDefault()
  }
}

// Todo / note shortcuts (⌘/Ctrl+K → new Todo, ⌘/Ctrl+Shift+N → new Note, both
// opening the item window; ⌘/Ctrl+Shift+K → the list drawer) are bound HERE, at
// the app root, rather than on the mascot: the mascot only mounts when it's
// enabled and in in-app mode, which used to leave the shortcut silently dead for
// anyone who had turned the fox off or moved it to the desktop-pet window.
//
// ⌘/Ctrl+B (hide/show the sidebar) rides along here for the same reason: it has
// to work from every screen, including one where the sidebar is already hidden
// and its own toggle button is off-screen.
// Both go through `runMenuAction`, which de-duplicates against the identical
// menu accelerators (see appMenu.ts) so one press never fires twice.
function onQuickCaptureKey(e: KeyboardEvent) {
  if (!(e.metaKey || e.ctrlKey) || e.altKey) return
  const key = e.key.toLowerCase()
  if (key === 'k' && !e.shiftKey) {
    e.preventDefault()
    runMenuAction('file.newTodo')
  } else if (key === 'k' && e.shiftKey) {
    e.preventDefault()
    runMenuAction('view.itemPanel')
  } else if (key === 'n' && e.shiftKey) {
    e.preventDefault()
    runMenuAction('file.newNote')
  } else if (key === 'b' && !e.shiftKey) {
    e.preventDefault()
    runMenuAction('view.toggleSidebar')
  }
}

// The sidebar is gone either because the user hid it (any route, persisted) or
// because focus mode is collapsing a run down to its conversation.
const sidebarVisible = computed(
  () => !uiLayout.sidebarHidden && !(uiLayout.focusMode && isRunRoute.value),
)

// ── Native menu bar ─────────────────────────────────────────────────────────
// Every menu action that isn't screen-specific is bound here, at the app root,
// so it works from any route. RunView binds `file.newSession` itself — starting
// a session needs a project, which only that screen has.
function bindGlobalMenuActions() {
  const unbinds = [
    // Writing a todo / note ALWAYS happens in the standalone item window, from
    // every entry point — menu, shortcut, mascot, run screen.
    registerMenuAction('file.newTodo', () =>
      openItemCreateWindow('todo', routeCaptureContext()),
    ),
    registerMenuAction('file.newNote', () =>
      openItemCreateWindow('note', routeCaptureContext()),
    ),
    registerMenuAction('view.itemPanel', () =>
      itemPanel.togglePanel({ projectId: routeCaptureContext().projectId }),
    ),
    registerMenuAction('view.toggleSidebar', () => uiLayout.toggleSidebar()),
    registerMenuAction('view.toggleFocus', () => uiLayout.toggleFocus()),
    registerMenuAction('view.reload', () => window.location.reload()),
    ...NAV_ROUTES.map((r) => registerMenuAction(`go.${r.path}`, () => router.push(r.path))),
  ]
  return () => unbinds.forEach((off) => off())
}

let unbindMenuActions: (() => void) | null = null
let unlistenMenu: UnlistenFn | null = null

// Labels carry both the language AND the current layout state ("Hide" vs "Show
// Sidebar"), so the menu is rebuilt whenever either moves. Watching the i18n
// locale rather than the settings row avoids rebuilding with stale labels: the
// setting is what *drives* the switch, `locale` is what has actually applied.
watch(
  () => [locale.value, uiLayout.sidebarHidden, uiLayout.focusMode] as const,
  () => {
    if (isPopoutWindow) return
    applyAppMenu({ sidebarHidden: uiLayout.sidebarHidden, focusMode: uiLayout.focusMode })
  },
)

// File the capture under whatever the user is looking at, so capturing from a
// run needs no project picking at all.
function routeCaptureContext() {
  return {
    projectId: typeof route.params.projectId === 'string' ? route.params.projectId : null,
    runId: typeof route.params.runId === 'string' ? route.params.runId : null,
  }
}

onBeforeUnmount(() => {
  unlistenActivated?.()
  unlistenActivated = null
  unbindMenuActions?.()
  unbindMenuActions = null
  unlistenMenu?.()
  unlistenMenu = null
  window.removeEventListener('keydown', onGlobalNavKeyGuard, true)
  window.removeEventListener('keydown', onQuickCaptureKey)
})

// App version shown in the sidebar footer; read from Tauri so it always matches
// the packaged build (tauri.conf.json) instead of a hardcoded string.
const appVersion = ref('')
onMounted(async () => {
  window.addEventListener('keydown', onGlobalNavKeyGuard, true)
  try {
    appVersion.value = await getVersion()
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }
})

// Same source of truth as the native "Go" menu (see lib/navigation.ts).
const navItems = computed(() =>
  NAV_ROUTES.map((r) => ({ path: r.path, label: t(r.labelKey), icon: r.icon })),
)

const isDark = ref(false)

function applyTheme(theme: string) {
  if (theme === 'dark') {
    document.documentElement.classList.add('dark')
    isDark.value = true
  } else if (theme === 'light') {
    document.documentElement.classList.remove('dark')
    isDark.value = false
  } else {
    const dark = window.matchMedia('(prefers-color-scheme: dark)').matches
    document.documentElement.classList.toggle('dark', dark)
    isDark.value = dark
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (e) => {
      document.documentElement.classList.toggle('dark', e.matches)
      isDark.value = e.matches
    })
  }
}

// Named color palette. Sets the `data-theme` attribute on <html> (see main.css);
// the default (indigo) theme uses no attribute so it falls back to :root/.dark.
function applyColorTheme(theme: string) {
  const t = theme && theme !== 'default' ? theme : ''
  if (t) document.documentElement.setAttribute('data-theme', t)
  else document.documentElement.removeAttribute('data-theme')
}

onMounted(async () => {
  if (isPopoutWindow) {
    // Pop-out window: only theme matters; skip the main app's data fetches.
    try {
      await appSettings.refresh()
      applyTheme(appSettings.settings?.theme ?? 'system')
      applyColorTheme(appSettings.settings?.color_theme ?? 'default')
      setLocale(appSettings.settings?.language ?? 'en')
    } catch {
      applyTheme('system')
    }
    return
  }
  window.addEventListener('keydown', onQuickCaptureKey)

  // Native menu bar: bind the actions first, then install the menu, so an
  // impatient click on a freshly drawn item can't land on nothing.
  unbindMenuActions = bindGlobalMenuActions()
  try {
    unlistenMenu = await listenMenuActions()
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }
  applyAppMenu({ sidebarHidden: uiLayout.sidebarHidden, focusMode: uiLayout.focusMode })

  // Attach per-run listeners for any run the backend reports as active — even
  // ones this window never opened (started/resumed from a remote Controller, or
  // still live after a restart). This is what lets a remotely-driven run's
  // permission / question prompts surface on the desktop (active-runs dock +
  // native notification), not just on the phone. `startListening` is idempotent,
  // so re-announcing a locally-started run is a harmless no-op.
  try {
    unlistenActivated = await listen<{ run_id: string; project_id: string }>(
      'run:activated',
      (e) => {
        const { run_id, project_id } = e.payload ?? {}
        if (run_id && project_id) live.startListening(run_id, project_id).catch(() => {})
      },
    )
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // Load projects up front so app-wide UI (e.g. permission notifications) can
  // resolve project names without waiting for the Projects view to open.
  projectsStore.fetchProjects()
  projectsStore.fetchConflicts()
  projectsStore.fetchRuleConflicts()
  try {
    await appSettings.refresh()
    applyTheme(appSettings.settings?.theme ?? 'system')
    applyColorTheme(appSettings.settings?.color_theme ?? 'default')
    setLocale(appSettings.settings?.language ?? 'en')
  } catch {
    applyTheme('system')
  }
})
</script>

<template>
  <!-- Pop-out file viewer window: bare layout, no app chrome. -->
  <FileViewerWindow v-if="isFileWindow" />

  <!-- Pop-out permission prompt window: bare layout, mirrors the main window. -->
  <PermissionWindow v-else-if="isPermissionWindow" />

  <!-- THE todo / note window: create or edit, the only place either is written. -->
  <ItemWindow v-else-if="isItemWindow" />

  <!-- Pop-out Gantt window: bare Gantt chart on its own OS window. -->
  <IssuesGanttView v-else-if="isGanttWindow" />

  <!-- Desktop-pet window: transparent, frameless host that floats only the fox. -->
  <MascotWindow v-else-if="isMascotWindow" />

  <div v-else class="flex flex-col h-screen bg-background text-foreground overflow-hidden">
    <!-- Animated decorative overlay for scenic themes (e.g. Full Moon 🌕).
         Sits ABOVE the UI as a non-interactive, screen-blended light layer so it
         stays visible over the app's opaque panels without blocking clicks. -->
    <ThemeDecorations :active="sceneOn" :theme="sceneTheme" />

    <!-- Unified title bar: brand + open-project tabs + global actions on ONE 44px
         row spanning the whole window, above both columns.

         This is the app's only chrome row. The sidebar has no brand header of
         its own any more, so nothing competes with a view's header for the top
         line, and the run screen gains back ~52px of height for the conversation.
         Hidden in focus mode, where the point is no chrome at all. -->
    <header
      v-if="!(uiLayout.focusMode && isRunRoute)"
      class="flex items-center gap-2 h-11 px-3 shrink-0 bg-sidebar border-b border-border/50"
    >
      <img src="/logo.png" alt="Devdy" class="h-[18px] w-[18px] rounded shrink-0" />
      <p class="text-[13px] font-semibold leading-none tracking-tight shrink-0">Devdy</p>
      <div v-if="tabsStore.tabs.length > 0" class="h-4 w-px bg-border shrink-0 mx-1" aria-hidden="true" />

      <WorkspaceTabs />

      <!-- Sits here rather than inside the sidebar: a "hide" button that
           disappears along with the thing it hides can't bring it back. -->
      <Button
        variant="ghost"
        size="icon-sm"
        class="shrink-0 ml-auto"
        :title="`${uiLayout.sidebarHidden ? t('menu.showSidebar') : t('menu.hideSidebar')} (${IS_MAC ? '⌘B' : 'Ctrl+B'})`"
        :aria-label="uiLayout.sidebarHidden ? t('menu.showSidebar') : t('menu.hideSidebar')"
        @click="uiLayout.toggleSidebar()"
      >
        <component :is="uiLayout.sidebarHidden ? PanelLeftOpen : PanelLeftClose" class="h-4 w-4" :stroke-width="1.75" />
      </Button>
    </header>

    <div class="flex flex-1 min-h-0">
    <!-- Sidebar. Hidden either by the ⌘/Ctrl+B toggle (all routes, persisted) or
         by focus mode — the latter only while in the run workspace, so other
         routes like project settings keep their navigation. Once hidden it is
         gone entirely; View → Show Sidebar (⌘B) brings it back.

         It collapses by animating its own width to zero while the inner column
         keeps a fixed 220px, so the nav slides out of view instead of the labels
         reflowing into a squashed mess on the way. -->
    <Transition name="sidebar">
      <aside
        v-if="sidebarVisible"
        class="app-sidebar shrink-0 overflow-hidden bg-sidebar border-r border-border/50"
      >
        <div class="w-[220px] h-full flex flex-col">
          <!-- No brand header: the logo moved to the title bar, so the nav starts
               at the very top and a view's header owns the first line alone. -->
          <nav class="flex-1 px-2 py-2.5 space-y-0.5">
            <template v-for="item in navItems" :key="item.path">
              <!-- Todos & Notes is an overlay, not a destination: it opens the
                   app-wide drawer instead of navigating, so whatever screen (or
                   running session) is underneath survives untouched. It rides
                   just above Settings, so the settings/about pair stays last. -->
              <button
                v-if="item.path === '/settings'"
                type="button"
                class="relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors cursor-pointer select-none"
                :class="itemPanel.open.value
                  ? 'bg-accent text-foreground font-medium'
                  : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
                :title="`${t('item.panelTitle')} (${IS_MAC ? '⌘⇧K' : 'Ctrl+Shift+K'})`"
                @click="runMenuAction('view.itemPanel')"
              >
                <ListTodo class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                <span class="flex-1 truncate text-left">{{ t('item.panelTitle') }}</span>
              </button>

              <RouterLink
                :to="item.path"
                class="relative flex items-center gap-2.5 px-3 py-2 rounded-md text-sm transition-colors cursor-pointer select-none"
                :class="route.path.startsWith(item.path)
                  ? 'bg-accent text-foreground font-medium'
                  : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
              >
                <span
                  v-if="route.path.startsWith(item.path)"
                  class="absolute left-0 inset-y-[6px] w-[2px] rounded-r-full bg-primary"
                />
                <component :is="item.icon" class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                <span class="flex-1 truncate">{{ item.label }}</span>
                <span
                  v-if="item.path === '/projects' && (projectsStore.conflicts.length + projectsStore.ruleConflicts.length) > 0"
                  class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-amber-500 px-1 text-[10px] font-medium text-white leading-none"
                >
                  {{ projectsStore.conflicts.length + projectsStore.ruleConflicts.length }}
                </span>
              </RouterLink>
            </template>
          </nav>

          <!-- App-wide monitor of concurrent runs + permission center -->
          <ActiveRunsDock />

          <!-- Active Remote Control session (phone paired to a run) -->
          <ActiveRemoteDock />

          <!-- Version footer -->
          <div class="px-4 py-3 border-t border-border/50">
            <p v-if="appVersion" class="text-[10px] text-muted-foreground/50 font-mono">v{{ appVersion }}</p>
          </div>
        </div>
      </aside>
    </Transition>

    <!-- Main content -->
    <main class="flex-1 min-w-0 flex flex-col overflow-hidden">
      <div class="flex-1 min-w-0 overflow-auto">
        <!-- `:key` stays on the inner <component>, NOT on <RouterView>: KeepAlive
             caches entries by their vnode key, and a key on the RouterView would
             tear the whole cache down on every navigation — the opposite of the
             point. `routeKey` keeps its existing contract either way (see above):
             run routes get `run-<projectId>` so RunView still remounts per
             project, other routes get a stable per-view key. -->
        <RouterView v-slot="{ Component }">
          <KeepAlive :include="CACHED_VIEWS" :max="8">
            <component :is="Component" :key="routeKey" />
          </KeepAlive>
        </RouterView>
      </div>
    </main>
    </div>

    <!-- Headless: fires native OS notifications for runs awaiting input while the
         app is backgrounded; the in-app signal is the History attention icon. -->
    <PermissionNotifier />

    <!-- Headless: fires native reminders for upcoming calendar events app-wide;
         clicking opens the event's detail drawer on the Calendar screen. -->
    <CalendarReminder />

    <!-- App-wide confirm dialog host (see useConfirm) -->
    <ConfirmModal />

    <!-- App-wide text-input dialog host (see usePrompt) -->
    <PromptModal />

    <!-- App-wide toast host (see useToast) -->
    <ToastHost />

    <!-- App-wide side-by-side image comparison (banner + picker + compare view) -->
    <ImageCompareHost />

    <!-- DY mascot: app-wide operator. Host picks in-app floating vs desktop pet. -->
    <CyberFoxHost />

    <!-- App-wide Todos & Notes list (⌘⇧K). An overlay, never a route change, so
         a run in progress keeps its scroll and panel state. It replaced the two
         list screens; writing happens in the item window. -->
    <ItemListDrawer />
  </div>
</template>

<style scoped>
/* Sidebar collapse/expand. Width is set here rather than as a Tailwind class so
   the transition classes below can override it without a specificity fight. */
.app-sidebar {
  width: 220px;
}

.sidebar-enter-active,
.sidebar-leave-active {
  transition: width 180ms ease, opacity 180ms ease;
}

.sidebar-enter-from,
.sidebar-leave-to {
  width: 0;
  /* The border would otherwise linger as a 1px line at the very end. */
  border-right-width: 0;
  opacity: 0;
}

@media (prefers-reduced-motion: reduce) {
  .sidebar-enter-active,
  .sidebar-leave-active {
    transition: none;
  }
}
</style>
