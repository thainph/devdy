<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { RouterLink, RouterView, useRoute } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useProjectsStore } from '@/stores/projects'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useUILayoutStore } from '@/stores/uiLayout'
import { useI18n } from 'vue-i18n'
import { setLocale } from '@/i18n'
import { Puzzle, ScrollText, Server, HardDrive, FolderOpen, GitPullRequest, GanttChartSquare, BarChart3, CalendarClock, CalendarDays, ListTodo, StickyNote, Settings, Info } from 'lucide-vue-next'
import PermissionNotifier from '@/components/PermissionNotifier.vue'
import CalendarReminder from '@/components/CalendarReminder.vue'
import BudgetBadge from '@/components/BudgetBadge.vue'
import WorkspaceTabs from '@/components/WorkspaceTabs.vue'
import ActiveRunsDock from '@/components/ActiveRunsDock.vue'
import ActiveRemoteDock from '@/components/ActiveRemoteDock.vue'
import FileViewerWindow from '@/views/FileViewerWindow.vue'
import PermissionWindow from '@/views/PermissionWindow.vue'
import ThemeDecorations from '@/components/ThemeDecorations.vue'
import { ConfirmModal, PromptModal, ToastHost } from '@/components/ui'
import ImageCompareHost from '@/components/ImageCompareHost.vue'
import CyberFoxHost from '@/components/CyberFoxHost.vue'
import MascotWindow from '@/views/MascotWindow.vue'
import QuickCreateWindow from '@/views/QuickCreateWindow.vue'
import QuickCapturePanel from '@/components/QuickCapturePanel.vue'
import { useQuickCapture } from '@/composables/useQuickCapture'
import IssuesGanttView from '@/views/IssuesGanttView.vue'
import { getVersion } from '@tauri-apps/api/app'

// Pop-out windows load the same SPA with a query flag; render a bare,
// chrome-less host (no sidebar / nav / background work) in those cases.
const isFileWindow = new URLSearchParams(window.location.search).get('fileWindow') === '1'
const isPermissionWindow = new URLSearchParams(window.location.search).get('permissionWindow') === '1'
const isQuickCreateWindow = new URLSearchParams(window.location.search).get('quickCreateWindow') === '1'
const isGanttWindow = new URLSearchParams(window.location.search).get('ganttWindow') === '1'
const isMascotWindow = new URLSearchParams(window.location.search).get('mascotWindow') === '1'
// All pop-out kinds only need the theme applied; skip the main app's data work.
const isPopoutWindow =
  isFileWindow || isPermissionWindow || isQuickCreateWindow || isGanttWindow || isMascotWindow

const route = useRoute()
const projectsStore = useProjectsStore()
const appSettings = useAppSettingsStore()
const tabsStore = useWorkspaceTabsStore()
const uiLayout = useUILayoutStore()
const live = useLiveRunsStore()
const { t } = useI18n()
const { openCapture } = useQuickCapture()

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

// Quick capture (⌘/Ctrl+K → Todo, ⌘/Ctrl+Shift+N → Note) is bound HERE, at the
// app root, rather than on the mascot: the mascot only mounts when it's enabled
// and in in-app mode, which used to leave the shortcut silently dead for anyone
// who had turned the fox off or moved it to the desktop-pet window.
function onQuickCaptureKey(e: KeyboardEvent) {
  if (!(e.metaKey || e.ctrlKey) || e.altKey) return
  const key = e.key.toLowerCase()
  if (key === 'k' && !e.shiftKey) {
    e.preventDefault()
    openCapture({ tab: 'todo', context: routeCaptureContext() })
  } else if (key === 'n' && e.shiftKey) {
    e.preventDefault()
    openCapture({ tab: 'note', context: routeCaptureContext() })
  }
}

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

const navItems = computed(() => [
  { path: '/projects', label: t('nav.projects'), icon: FolderOpen },
  { path: '/pr-inbox', label: t('nav.prInbox'), icon: GitPullRequest },
  { path: '/gantt', label: t('nav.gantt'), icon: GanttChartSquare },
  { path: '/skills', label: t('nav.skills'), icon: Puzzle },
  { path: '/rules', label: t('nav.rules'), icon: ScrollText },
  { path: '/mcp', label: t('nav.mcp'), icon: Server },
  { path: '/servers', label: t('nav.servers'), icon: HardDrive },
  { path: '/stats', label: t('nav.stats'), icon: BarChart3 },
  { path: '/work-digest', label: t('nav.digest'), icon: CalendarClock },
  { path: '/calendar', label: t('nav.calendar'), icon: CalendarDays },
  { path: '/todos', label: t('nav.todos'), icon: ListTodo },
  { path: '/notes', label: t('nav.notes'), icon: StickyNote },
  { path: '/settings', label: t('nav.settings'), icon: Settings },
  { path: '/about', label: t('nav.about'), icon: Info },
])

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

  <!-- Pop-out quick-create window: bare Todo/Note form, writes to shared DB. -->
  <QuickCreateWindow v-else-if="isQuickCreateWindow" />

  <!-- Pop-out Gantt window: bare Gantt chart on its own OS window. -->
  <IssuesGanttView v-else-if="isGanttWindow" />

  <!-- Desktop-pet window: transparent, frameless host that floats only the fox. -->
  <MascotWindow v-else-if="isMascotWindow" />

  <div v-else class="flex h-screen bg-background text-foreground overflow-hidden">
    <!-- Animated decorative overlay for scenic themes (e.g. Full Moon 🌕).
         Sits ABOVE the UI as a non-interactive, screen-blended light layer so it
         stays visible over the app's opaque panels without blocking clicks. -->
    <ThemeDecorations :active="sceneOn" :theme="sceneTheme" />
    <!-- Sidebar (hidden in focus mode, but only while in the run workspace so
         other routes like project settings keep their navigation) -->
    <aside v-if="!(uiLayout.focusMode && isRunRoute)" class="w-[220px] shrink-0 flex flex-col bg-sidebar border-r border-border/50">
      <!-- Brand -->
      <div class="flex items-center gap-2.5 px-4 h-[52px] border-b border-border/50">
        <img src="/logo.png" alt="Devdy" class="h-6 w-6 rounded shrink-0" />
        <div class="min-w-0">
          <p class="text-sm font-semibold leading-none tracking-tight">Devdy</p>
          <p class="text-[10px] text-muted-foreground mt-0.5 leading-none">{{ t('nav.tagline') }}</p>
        </div>
      </div>

      <!-- Nav items -->
      <nav class="flex-1 px-2 py-2.5 space-y-0.5">
        <RouterLink
          v-for="item in navItems"
          :key="item.path"
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
      </nav>

      <!-- App-wide monitor of concurrent runs + permission center -->
      <ActiveRunsDock />

      <!-- Active Remote Control session (phone paired to a run) -->
      <ActiveRemoteDock />

      <!-- Global usage / budget status -->
      <BudgetBadge />

      <!-- Version footer -->
      <div class="px-4 py-3 border-t border-border/50">
        <p v-if="appVersion" class="text-[10px] text-muted-foreground/50 font-mono">v{{ appVersion }}</p>
      </div>
    </aside>

    <!-- Main content -->
    <main class="flex-1 min-w-0 flex flex-col overflow-hidden">
      <!-- Open-run tabs (only in the run workspace) -->
      <WorkspaceTabs v-if="isRunRoute && !uiLayout.focusMode" />
      <div class="flex-1 min-w-0 overflow-auto">
        <RouterView :key="routeKey" />
      </div>
    </main>

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

    <!-- App-wide quick capture (⌘K Todo / ⌘⇧N Note). An overlay, never a route
         change, so a run in progress keeps its scroll and panel state. -->
    <QuickCapturePanel />
  </div>
</template>
