<script setup lang="ts">
// Renders one surface tab (PR Inbox, Calendar, Slack threads, Todos, a file…)
// inside the main window: one per open tab in the left pane (App.vue shows the
// routed one) or in the right half of a split. The same views as the pop-out
// windows, in their in-app mode.
import { computed } from 'vue'
import router from '@/router'
import { surfaceLocation, surfaceTabId, type SurfaceKind, type SurfaceParams } from '@/lib/surfaces'
import { closeSurfaceTab, openSurface, surfaceTabIdOfRoute } from '@/lib/surfaceOpen'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import PrInboxView from '@/views/PrInboxView.vue'
import GithubMentionsView from '@/views/GithubMentionsView.vue'
import CalendarView from '@/views/CalendarView.vue'
import DeliveryRadarView from '@/views/DeliveryRadarView.vue'
import ItemListWindow from '@/views/ItemListWindow.vue'
import SlackThreadsWindow from '@/views/SlackThreadsWindow.vue'
import FileViewerWindow from '@/views/FileViewerWindow.vue'

const props = defineProps<{
  surface: SurfaceKind
  params: SurfaceParams
}>()

const tabs = useWorkspaceTabsStore()
const line = computed(() => (props.params.line ? Number(props.params.line) : null))
const tabId = computed(() => surfaceTabId(props.surface, props.params))

// File viewer (embedded): it can't close or open windows itself.
function onFileClose() {
  void closeSurfaceTab(tabId.value)
}
// A link in a file opens the new file as a tab (the old one stays a tab; a
// shown split stays fixed, so it opens full width). ⌘/Ctrl-click opens its own
// window.
function onOpenFile(path: string, at: number | null, newWindow: boolean) {
  void openSurface(
    'file',
    { projectPath: props.params.projectPath, path, line: at ?? undefined },
    { placement: newWindow ? 'window' : 'tab' },
  )
}
// A rename can arrive while the tab is hidden: re-point it in place, and move
// the route along only if the router was showing it.
function onRenamed(path: string) {
  const old = tabId.value
  const routed = surfaceTabIdOfRoute() === old
  const tab = tabs.retargetSurface(old, { ...props.params, path })
  if (tab && routed) void router.replace(surfaceLocation('file', tab.params, true)).catch(() => {})
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <PrInboxView v-if="surface === 'prInbox'" class="min-h-0 flex-1" />
    <GithubMentionsView v-else-if="surface === 'mentions'" class="min-h-0 flex-1" />
    <CalendarView v-else-if="surface === 'calendar'" class="min-h-0 flex-1" />
    <DeliveryRadarView v-else-if="surface === 'radar'" :project="params.project" class="min-h-0 flex-1" />
    <ItemListWindow
      v-else-if="surface === 'todos' || surface === 'notes'"
      :key="surface"
      embedded
      :kind="surface === 'todos' ? 'todo' : 'note'"
      :initial-id="params.id ?? null"
      :project-id="params.projectId ?? null"
      class="min-h-0 flex-1"
    />
    <SlackThreadsWindow
      v-else-if="surface === 'slack'"
      embedded
      :initial-id="params.id ?? null"
      :project-id="params.projectId ?? null"
      class="min-h-0 flex-1"
    />
    <FileViewerWindow
      v-else-if="surface === 'file'"
      :key="`${params.projectPath}|${params.path}`"
      embedded
      :project-path="params.projectPath ?? ''"
      :path="params.path ?? ''"
      :line="line"
      class="min-h-0 flex-1"
      @close="onFileClose"
      @open-file="onOpenFile"
      @renamed="onRenamed"
    />
  </div>
</template>
