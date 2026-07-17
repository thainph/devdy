<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useProjectsStore } from '@/stores/projects'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'

/**
 * Headless, app-wide notifier for pending permission / question requests on runs
 * the user is NOT currently looking at. It renders nothing: in-app, the History
 * list shows an animated attention icon on the waiting run (see RunView), so the
 * only job here is firing a native OS notification when the app is in the
 * background. Clicking it focuses the window and routes to the waiting run, where
 * the in-place permission / question modal is shown.
 */

interface PendingRun {
  runId: string
  projectId: string
  request: PermissionRequest
}

const route = useRoute()
const router = useRouter()
const live = useLiveRunsStore()
const projectsStore = useProjectsStore()
const tabsStore = useWorkspaceTabsStore()

// The run currently open in RunView — its request is already shown there.
const activeRunId = computed(() =>
  typeof route.params.runId === 'string' ? route.params.runId : null,
)

const pending = computed<PendingRun[]>(() => {
  const out: PendingRun[] = []
  live.sessions.forEach((s) => {
    const req = s.permissionQueue[0]
    if (!req) return
    if (s.runId === activeRunId.value) return
    out.push({ runId: s.runId, projectId: s.projectId, request: req })
  })
  return out
})

function isQuestion(req: PermissionRequest): boolean {
  return req.tool_name === 'AskUserQuestion'
}

function label(req: PermissionRequest): string {
  if (isQuestion(req)) return 'Claude is asking you a question'
  if (req.tool_name === 'ExitPlanMode') return 'Claude finished planning — review the plan'
  return req.title || req.display_name || `Wants to run ${req.tool_name}`
}

function projectName(projectId: string): string {
  return projectsStore.projects.find((p) => p.id === projectId)?.name ?? 'Project'
}

function navigateToRun(projectId: string, runId: string) {
  // Register/open the project's workspace tab BEFORE routing — the run
  // workspace is keyed by projectId and driven by the tabs store, so a bare
  // router.push lands on an empty view when the tab isn't open yet.
  tabsStore.open(projectId, runId)
  router
    .push({ name: 'project-run-detail', params: { projectId, runId } })
    .catch(() => {})
}

// --- Native OS notifications ---------------------------------------------

let permissionGranted = false
let unlistenClick: UnlistenFn | null = null
const notified = new Set<string>()

onMounted(async () => {
  try {
    permissionGranted = await isPermissionGranted()
    if (!permissionGranted) {
      permissionGranted = (await requestPermission()) === 'granted'
    }
  } catch {
    permissionGranted = false
  }

  // Click handling is done natively (see src-tauri notifications.rs): the plugin's
  // `onAction` never fires on desktop, so the Rust side emits this event carrying
  // the run identity when the user clicks the notification.
  try {
    unlistenClick = await listen<{ projectId?: string; runId?: string }>(
      'permission-notification-clicked',
      (e) => {
        const { projectId, runId } = e.payload ?? {}
        getCurrentWindow()
          .setFocus()
          .catch(() => {})
        if (projectId && runId) {
          navigateToRun(projectId, runId)
        }
      },
    )
  } catch {
    unlistenClick = null
  }
})

onBeforeUnmount(() => {
  unlistenClick?.()
  unlistenClick = null
})

// Fire a native notification once per new pending request (not on the run the
// user is already viewing). Prune ids that have left every queue so the same
// request can re-alert only if it genuinely reappears.
watch(
  pending,
  (list) => {
    const allPendingIds = new Set<string>()
    live.sessions.forEach((s) => {
      for (const r of s.permissionQueue) allPendingIds.add(r.request_id)
    })
    for (const id of [...notified]) {
      if (!allPendingIds.has(id)) notified.delete(id)
    }

    if (!permissionGranted) return
    for (const t of list) {
      const id = t.request.request_id
      if (notified.has(id)) continue
      notified.add(id)
      // Fire via the native Rust command so notification clicks route back to
      // this run (the plugin's sendNotification has no working desktop click).
      invoke('show_permission_notification', {
        title: `${projectName(t.projectId)} needs permission`,
        body: label(t.request),
        projectId: t.projectId,
        runId: t.runId,
      }).catch(() => {
        // ignore — the History attention icon still covers it in-app
      })
    }
  },
  { deep: true },
)
</script>

<template>
  <!-- Headless: the in-app signal is the animated icon in RunView's History list. -->
  <span hidden aria-hidden="true" />
</template>
