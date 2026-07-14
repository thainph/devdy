<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { RouterLink, RouterView, useRoute } from 'vue-router'
import { useProjectsStore } from '@/stores/projects'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { Puzzle, ScrollText, FolderOpen, BarChart3, CalendarClock, Settings, Info } from 'lucide-vue-next'
import PermissionNotifier from '@/components/PermissionNotifier.vue'
import BudgetBadge from '@/components/BudgetBadge.vue'
import WorkspaceTabs from '@/components/WorkspaceTabs.vue'
import ActiveRunsDock from '@/components/ActiveRunsDock.vue'
import FileViewerWindow from '@/views/FileViewerWindow.vue'
import { ConfirmModal } from '@/components/ui'
import { getVersion } from '@tauri-apps/api/app'

// Pop-out file viewer windows load the same SPA with `?fileWindow=1`; render a
// bare, chrome-less viewer (no sidebar / nav / background work) in that case.
const isFileWindow = new URLSearchParams(window.location.search).get('fileWindow') === '1'

const route = useRoute()
const projectsStore = useProjectsStore()
const appSettings = useAppSettingsStore()
const tabsStore = useWorkspaceTabsStore()

const isRunRoute = computed(
  () => route.name === 'project-run' || route.name === 'project-run-detail',
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

// App version shown in the sidebar footer; read from Tauri so it always matches
// the packaged build (tauri.conf.json) instead of a hardcoded string.
const appVersion = ref('')
onMounted(async () => {
  try {
    appVersion.value = await getVersion()
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }
})

const navItems = [
  { path: '/projects', label: 'Projects', icon: FolderOpen },
  { path: '/skills', label: 'Skills', icon: Puzzle },
  { path: '/rules', label: 'Rules', icon: ScrollText },
  { path: '/stats', label: 'Stats', icon: BarChart3 },
  { path: '/work-digest', label: 'Digest', icon: CalendarClock },
  { path: '/settings', label: 'Settings', icon: Settings },
  { path: '/about', label: 'About', icon: Info },
]

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
  if (isFileWindow) {
    // Pop-out window: only theme matters; skip the main app's data fetches.
    try {
      await appSettings.refresh()
      applyTheme(appSettings.settings?.theme ?? 'system')
      applyColorTheme(appSettings.settings?.color_theme ?? 'default')
    } catch {
      applyTheme('system')
    }
    return
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
  } catch {
    applyTheme('system')
  }
})
</script>

<template>
  <!-- Pop-out file viewer window: bare layout, no app chrome. -->
  <FileViewerWindow v-if="isFileWindow" />

  <div v-else class="flex h-screen bg-background text-foreground overflow-hidden">
    <!-- Sidebar -->
    <aside class="w-[220px] shrink-0 flex flex-col bg-sidebar border-r border-border/50">
      <!-- Brand -->
      <div class="flex items-center gap-2.5 px-4 h-[52px] border-b border-border/50">
        <img src="/logo.png" alt="Devdy" class="h-6 w-6 rounded shrink-0" />
        <div class="min-w-0">
          <p class="text-sm font-semibold leading-none tracking-tight">Devdy</p>
          <p class="text-[10px] text-muted-foreground mt-0.5 leading-none">AI coding agent for developers</p>
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
      <WorkspaceTabs v-if="isRunRoute" />
      <div class="flex-1 min-w-0 overflow-auto">
        <RouterView :key="routeKey" />
      </div>
    </main>

    <!-- Headless: fires native OS notifications for runs awaiting input while the
         app is backgrounded; the in-app signal is the History attention icon. -->
    <PermissionNotifier />

    <!-- App-wide confirm dialog host (see useConfirm) -->
    <ConfirmModal />
  </div>
</template>
