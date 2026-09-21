<script setup lang="ts">
/**
 * Worker panel for a conductor session — the only conductor-specific UI. It sits
 * in the run viewer's right sidebar and shows the worker tree (with a "needs
 * permission" flag) plus the control-plane event timeline. Everything else about
 * a conductor is just the normal session screen.
 *
 * Clicking a worker opens that worker's own session (it is an ordinary run), so
 * its permission prompt / stream shows in the standard viewer.
 */
import { useI18n } from 'vue-i18n'
import { Cpu, ExternalLink, ShieldQuestion } from 'lucide-vue-next'
import { useLiveRunsStore } from '@/stores/liveRuns'
import type { ConductorWorker, ConductorEvent } from '@/stores/conductor'
import { StatusBadge } from '@/components/ui'

defineProps<{ workers: ConductorWorker[]; events: ConductorEvent[] }>()
const emit = defineEmits<{ openSession: [runId: string] }>()

const { t } = useI18n()
const live = useLiveRunsStore()

function hasPending(workerId: string): boolean {
  return !!live.get(workerId)?.permissionQueue?.length
}
function shortId(id: string): string {
  return id.slice(0, 8)
}
function eventSummary(e: ConductorEvent): string {
  const p = (e.payload ?? {}) as Record<string, unknown>
  switch (e.kind) {
    case 'spawn':
      return t('conductor.event.spawn', { role: String(p.role_label ?? 'worker'), id: shortId(String(p.worker_id ?? '')) })
    case 'send':
      return t('conductor.event.send', { id: shortId(String(p.worker_id ?? '')) })
    case 'cancel':
      return t('conductor.event.cancel', { id: shortId(String(p.worker_id ?? '')) })
    default:
      return e.kind
  }
}
function formatWhen(iso: string): string {
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}
</script>

<template>
  <div class="flex h-full flex-col overflow-hidden">
    <!-- Workers -->
    <div class="flex-1 min-h-0 flex flex-col overflow-hidden">
      <div class="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground border-b border-border/40 shrink-0">
        {{ t('conductor.workers') }}
      </div>
      <div class="flex-1 min-h-0 overflow-y-auto">
        <p v-if="!workers.length" class="px-3 py-6 text-center text-[11px] text-muted-foreground">
          {{ t('conductor.noWorkers') }}
        </p>
        <button
          v-for="w in workers"
          :key="w.worker_id"
          class="group w-full text-left px-3 py-2 border-b border-border/30 hover:bg-accent/40 transition-colors cursor-pointer"
          @click="emit('openSession', w.worker_id)"
        >
          <div class="flex items-center gap-1.5">
            <Cpu class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.75" />
            <span class="flex-1 min-w-0 truncate text-[12px] font-medium leading-tight" :title="w.title ?? w.worker_id">
              {{ w.title ?? shortId(w.worker_id) }}
            </span>
            <ShieldQuestion
              v-if="hasPending(w.worker_id)"
              class="h-3.5 w-3.5 text-amber-500 shrink-0"
              :title="t('conductor.needsPermission')"
            />
            <StatusBadge :status="w.status" size="xs" />
          </div>
          <div class="mt-0.5 flex items-center gap-1.5 text-[10px] text-muted-foreground">
            <span class="font-mono">{{ w.engine }}</span>
            <span>·</span>
            <span class="font-mono">{{ shortId(w.worker_id) }}</span>
            <ExternalLink class="h-3 w-3 ml-auto opacity-0 group-hover:opacity-100 transition-opacity" :stroke-width="1.75" />
          </div>
        </button>
      </div>
    </div>

    <!-- Timeline -->
    <div class="h-48 shrink-0 flex flex-col overflow-hidden border-t border-border">
      <div class="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground border-b border-border/40 shrink-0">
        {{ t('conductor.timeline') }}
      </div>
      <div class="flex-1 min-h-0 overflow-y-auto px-3 py-2 space-y-1">
        <p v-if="!events.length" class="text-[11px] text-muted-foreground text-center py-4">
          {{ t('conductor.noEvents') }}
        </p>
        <div v-for="(e, i) in events" :key="i" class="flex items-start gap-2 text-[11px]">
          <span class="font-mono text-muted-foreground/70 shrink-0">{{ formatWhen(e.ts) }}</span>
          <span class="text-foreground/80">{{ eventSummary(e) }}</span>
        </div>
      </div>
    </div>
  </div>
</template>
