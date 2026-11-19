<script setup lang="ts">
// One tracked entry on the Processes screen: a row of the shared column grid
// (name + project, kind, state, process count, CPU / RAM with a bar against the
// heaviest entry, age, actions) that expands to the per-process table. The
// whole row toggles; its buttons don't. Pure presentation — the screen owns
// confirmation and the backend calls.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronRight, ExternalLink, Square, Skull, X } from 'lucide-vue-next'
import { Badge, Button } from '@/components/ui'
import ProcessKindBadge from './ProcessKindBadge.vue'
import ProcessStateBadge from './ProcessStateBadge.vue'
import UsageBar from './UsageBar.vue'
import { PROC_GRID } from './grid'
import {
  formatAge,
  formatCpu,
  formatRss,
  truncateCommand,
  type BgEntry,
} from '@/lib/backgroundProcesses'

const props = defineProps<{
  entry: BgEntry
  /** Wall clock for the age column; the screen ticks it with each refresh. */
  now: number
  /** Heaviest CPU / RAM among the listed entries: the bars' full scale. */
  cpuMax: number
  rssMax: number
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
const stray = computed(() => props.entry.state !== 'active')

function toggle() {
  expanded.value = !expanded.value
}
</script>

<template>
  <div>
    <div
      class="group relative grid cursor-pointer items-center gap-3 px-3 py-1.5 transition-colors hover:bg-accent/40 focus-visible:bg-accent/40 focus-visible:outline-none"
      :class="PROC_GRID"
      role="button"
      tabindex="0"
      :aria-expanded="expanded"
      :aria-label="expanded ? t('processes.actions.collapse') : t('processes.actions.expand')"
      @click="toggle"
      @keydown.enter.self.prevent="toggle"
      @keydown.space.self.prevent="toggle"
    >
      <span v-if="stray" class="absolute inset-y-0 left-0 w-0.5 bg-warning" aria-hidden="true" />

      <div class="flex min-w-0 items-center gap-2">
        <ChevronRight
          class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
          :class="expanded && 'rotate-90'"
          :stroke-width="2"
          aria-hidden="true"
        />
        <div class="min-w-0">
          <span class="block truncate text-[13px] leading-tight" :title="entry.label">{{ entry.label }}</span>
          <span class="block truncate text-[11px] leading-tight text-muted-foreground">
            {{ entry.project_name || t('processes.noProject') }}
          </span>
        </div>
      </div>

      <div><ProcessKindBadge :kind="entry.kind" /></div>
      <div><ProcessStateBadge :state="entry.state" /></div>
      <span class="text-right text-[11px] tabular-nums text-muted-foreground">{{ entry.processes.length }}</span>
      <UsageBar :value="entry.cpu" :max="cpuMax" :text="formatCpu(entry.cpu)" :tone="entry.cpu >= 70 ? 'warning' : 'success'" />
      <UsageBar :value="entry.rss_kb" :max="rssMax" :text="formatRss(entry.rss_kb)" tone="primary" />
      <span class="text-right text-[11px] tabular-nums text-muted-foreground">{{ formatAge(entry.created_at, now) }}</span>

      <div
        class="flex items-center justify-end gap-1 opacity-60 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
        @click.stop
        @keydown.stop
      >
        <Button
          v-if="entry.run_id && !stray"
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
    <div v-if="expanded" class="bg-muted/30 px-3 pb-2 pl-9">
      <table class="w-full table-fixed text-[11px]">
        <thead class="text-muted-foreground">
          <tr class="border-b border-border/50">
            <th class="w-24 py-1 text-left font-medium">{{ t('processes.col.pid') }}</th>
            <th class="py-1 text-left font-medium">{{ t('processes.col.command') }}</th>
            <th class="w-14 py-1 text-right font-medium">{{ t('processes.col.cpu') }}</th>
            <th class="w-18 py-1 text-right font-medium">{{ t('processes.col.ram') }}</th>
            <th class="w-16 py-1 text-right font-medium">{{ t('processes.col.age') }}</th>
            <th class="w-9 py-1" />
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in entry.processes" :key="p.pid" class="border-b border-border/30 last:border-0">
            <td class="py-1 font-mono tabular-nums">
              {{ p.pid }}
              <Badge v-if="p.is_root" tone="neutral" size="xs" class="ml-1 align-middle">{{ t('processes.rootProcess') }}</Badge>
            </td>
            <td class="truncate py-1 pr-2 font-mono" :title="p.command">{{ truncateCommand(p.command, 160) }}</td>
            <td class="py-1 text-right tabular-nums">{{ formatCpu(p.cpu) }}</td>
            <td class="py-1 text-right tabular-nums">{{ formatRss(p.rss_kb) }}</td>
            <td class="py-1 text-right tabular-nums text-muted-foreground">{{ formatAge(p.started_at, now) }}</td>
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
