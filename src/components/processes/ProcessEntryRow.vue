<script setup lang="ts">
// One tracked entry on the Processes screen: summary line (kind, label, project,
// process count, CPU, RAM, age) with its actions, expanding to the per-process
// table. Pure presentation — the screen owns confirmation and the backend calls.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronRight, ExternalLink, Square, Skull, X } from 'lucide-vue-next'
import { Badge, Button } from '@/components/ui'
import ProcessKindBadge from './ProcessKindBadge.vue'
import {
  formatAge,
  formatCpu,
  formatRss,
  truncateCommand,
  type BgEntry,
} from '@/lib/backgroundProcesses'

defineProps<{
  entry: BgEntry
  /** Wall clock for the age column; the screen ticks it with each refresh. */
  now: number
  /** An action on this entry is in flight. */
  busy?: boolean
}>()

const emit = defineEmits<{
  stop: []
  force: []
  open: []
  killPid: [pid: number]
}>()

const { t } = useI18n()
const expanded = ref(false)
</script>

<template>
  <div>
    <div class="group flex items-center gap-2 px-3 py-2 hover:bg-accent/40 transition-colors">
      <button
        type="button"
        class="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent cursor-pointer"
        :title="expanded ? t('processes.actions.collapse') : t('processes.actions.expand')"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        <ChevronRight class="h-3.5 w-3.5 transition-transform" :class="expanded && 'rotate-90'" :stroke-width="2" />
      </button>

      <ProcessKindBadge :kind="entry.kind" />

      <button type="button" class="min-w-0 flex-1 text-left cursor-pointer" @click="expanded = !expanded">
        <span class="block truncate text-[13px] leading-tight" :title="entry.label">{{ entry.label }}</span>
        <span class="block truncate text-[11px] text-muted-foreground leading-tight">
          {{ entry.project_name || t('processes.noProject') }}
        </span>
      </button>

      <span class="w-24 shrink-0 text-right text-[11px] text-muted-foreground tabular-nums">
        {{ t('processes.processCount', { n: entry.processes.length }, entry.processes.length) }}
      </span>
      <span class="w-12 shrink-0 text-right text-[11px] tabular-nums" :title="t('processes.col.cpu')">{{ formatCpu(entry.cpu) }}</span>
      <span class="w-16 shrink-0 text-right text-[11px] tabular-nums" :title="t('processes.col.ram')">{{ formatRss(entry.rss_kb) }}</span>
      <span class="w-14 shrink-0 text-right text-[11px] text-muted-foreground tabular-nums" :title="t('processes.col.age')">
        {{ formatAge(entry.created_at, now) }}
      </span>

      <div class="flex shrink-0 items-center gap-1 pl-1">
        <Button
          v-if="entry.run_id"
          variant="ghost"
          size="icon-sm"
          :title="t('processes.actions.openSession')"
          :aria-label="t('processes.actions.openSession')"
          @click="emit('open')"
        >
          <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button variant="outline" size="xs" :disabled="busy" :title="t('processes.actions.stopTitle')" @click="emit('stop')">
          <Square class="h-3 w-3" :stroke-width="2" />
          {{ t('processes.actions.stop') }}
        </Button>
        <Button
          variant="destructive-ghost"
          size="icon-sm"
          :disabled="busy"
          :title="t('processes.actions.forceKillTitle')"
          :aria-label="t('processes.actions.forceKill')"
          @click="emit('force')"
        >
          <Skull class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </div>
    </div>

    <!-- Per-process table -->
    <div v-if="expanded" class="px-3 pb-2 pl-11">
      <table class="w-full table-fixed text-[11px]">
        <thead class="text-muted-foreground">
          <tr class="border-b border-border/50">
            <th class="w-16 py-1 text-left font-medium">{{ t('processes.col.pid') }}</th>
            <th class="py-1 text-left font-medium">{{ t('processes.col.command') }}</th>
            <th class="w-12 py-1 text-right font-medium">{{ t('processes.col.cpu') }}</th>
            <th class="w-16 py-1 text-right font-medium">{{ t('processes.col.ram') }}</th>
            <th class="w-14 py-1 text-right font-medium">{{ t('processes.col.age') }}</th>
            <th class="w-9 py-1" />
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in entry.processes" :key="p.pid" class="border-b border-border/30 last:border-0">
            <td class="py-1 font-mono tabular-nums">
              {{ p.pid }}
              <Badge v-if="p.is_root" tone="neutral" size="xs" class="ml-1 align-middle">{{ t('processes.rootProcess') }}</Badge>
            </td>
            <td class="py-1 pr-2 font-mono truncate" :title="p.command">{{ truncateCommand(p.command, 120) }}</td>
            <td class="py-1 text-right tabular-nums">{{ formatCpu(p.cpu) }}</td>
            <td class="py-1 text-right tabular-nums">{{ formatRss(p.rss_kb) }}</td>
            <td class="py-1 text-right text-muted-foreground tabular-nums">{{ formatAge(p.started_at, now) }}</td>
            <td class="py-0.5 text-right">
              <Button
                variant="destructive-ghost"
                size="icon-sm"
                :disabled="busy"
                :title="t('processes.actions.stopProcess')"
                :aria-label="t('processes.actions.stopProcess')"
                @click="emit('killPid', p.pid)"
              >
                <X class="h-3.5 w-3.5" :stroke-width="2" />
              </Button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
