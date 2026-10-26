<script setup lang="ts">
// Sidebar warning row under the Active runs dock: shown only while processes are
// left behind (by an ended session or a previous app run), linking to the
// Processes screen. Counts come from the `procs:changed` push, so it costs nothing
// while hidden.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'
import { RouterLink } from 'vue-router'
import { Cog } from 'lucide-vue-next'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'

const { t } = useI18n()
const route = useRoute()
const store = useBackgroundProcessesStore()
const n = computed(() => store.stray)
</script>

<template>
  <div v-if="n > 0" class="px-2 pb-2 border-t border-border/50 pt-2">
    <RouterLink
      to="/processes"
      class="flex items-center gap-2 py-1.5 px-2 rounded-md text-[12px] transition-colors cursor-pointer select-none text-amber-600 dark:text-amber-400 hover:bg-amber-500/10"
      :class="route.path === '/processes' && 'bg-amber-500/10'"
      :title="t('processes.dock.title')"
    >
      <Cog class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
      <span class="flex-1 truncate">{{ t('processes.dock.label', { n }, n) }}</span>
    </RouterLink>
  </div>
</template>
