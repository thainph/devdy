<script setup lang="ts">
// Unified "back to AI run" affordance used across secondary project screens
// (Issues, Settings). Resumes the last-viewed run when one is remembered.
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { Button } from '@/components/ui'
import { ArrowLeft } from 'lucide-vue-next'

const route = useRoute()
const router = useRouter()
const tabsStore = useWorkspaceTabsStore()
const projectId = computed(() => route.params.projectId as string)

function goToRun() {
  const lastRunId = tabsStore.tabs.find((t) => t.projectId === projectId.value)?.lastRunId
  if (lastRunId) {
    router
      .push({ name: 'project-run-detail', params: { projectId: projectId.value, runId: lastRunId } })
      .catch(() => {})
  } else {
    router.push({ name: 'project-run', params: { projectId: projectId.value } }).catch(() => {})
  }
}
</script>

<template>
  <Button variant="ghost" size="sm" title="Back to AI run" @click="goToRun">
    <ArrowLeft class="h-3.5 w-3.5" :stroke-width="2" />
    Run AI
  </Button>
</template>
