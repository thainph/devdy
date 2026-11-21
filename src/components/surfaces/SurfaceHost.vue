<script setup lang="ts">
// Renders one surface (PR Inbox, Calendar, Slack threads, Todos, a file…)
// inside the main window: as the routed tab (views/SurfaceRouteView) or in the
// right half of a split (App.vue). The same views as the pop-out windows, in
// their in-app mode.
import { computed } from 'vue'
import { surfaceTabId, type SurfaceKind, type SurfaceParams } from '@/lib/surfaces'
import { closeSurfaceTab, openSurface } from '@/lib/surfaceOpen'
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
  /** Shown in the right half of a split (files opened from here stay there). */
  inSplit?: boolean
}>()

const tabs = useWorkspaceTabsStore()
const line = computed(() => (props.params.line ? Number(props.params.line) : null))
const tabId = computed(() => surfaceTabId(props.surface, props.params))

// File viewer (embedded): it can't close or open windows itself.
function onFileClose() {
  void closeSurfaceTab(tabId.value)
}
function onOpenFile(path: string, at: number | null) {
  void openSurface(
    'file',
    { projectPath: props.params.projectPath, path, line: at ?? undefined },
    { placement: 'tab', split: props.inSplit },
  )
}
async function onRenamed(path: string) {
  const old = tabId.value
  await openSurface('file', { projectPath: props.params.projectPath, path }, { placement: 'tab', split: props.inSplit })
  if (surfaceTabId('file', { projectPath: props.params.projectPath ?? '', path }) !== old) tabs.closeSurface(old)
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
