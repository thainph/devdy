<script setup lang="ts">
// Compact read-only list of tracked entries (kind, label, project, process
// count, RAM) for the startup orphan prompt and the quit dialog.
import { useI18n } from 'vue-i18n'
import ProcessKindBadge from './ProcessKindBadge.vue'
import { formatRss, type BgEntry } from '@/lib/backgroundProcesses'

defineProps<{ entries: BgEntry[] }>()
const { t } = useI18n()
</script>

<template>
  <ul class="max-h-48 overflow-y-auto rounded-md border border-border/60 divide-y divide-border/50">
    <li v-for="e in entries" :key="e.id" class="flex items-center gap-2 px-3 py-1.5">
      <ProcessKindBadge :kind="e.kind" />
      <span class="min-w-0 flex-1">
        <span class="block truncate text-[13px] leading-tight" :title="e.label">{{ e.label }}</span>
        <span class="block truncate text-[11px] text-muted-foreground leading-tight">
          {{ e.project_name || t('processes.noProject') }}
        </span>
      </span>
      <span class="shrink-0 text-[11px] text-muted-foreground tabular-nums">
        {{ t('processes.processCount', { n: e.processes.length }, e.processes.length) }}
      </span>
      <span class="w-16 shrink-0 text-right text-[11px] tabular-nums">{{ formatRss(e.rss_kb) }}</span>
    </li>
  </ul>
</template>
