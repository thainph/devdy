<script setup lang="ts">
// Stat card "Today": runs finished today, how many failed, and the three
// latest finished runs (not limited to today; click opens the run).
import { useI18n } from 'vue-i18n'
import { CheckCircle2, CircleCheckBig, XCircle } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import type { RecentRun, RuntimeToday } from '@/lib/runtimeSnapshot'

defineProps<{ today: RuntimeToday | null }>()
const emit = defineEmits<{ open: [run: RecentRun]; all: [] }>()
const { t } = useI18n()

/** Time of day; the date too when it is not today (the list is not limited to today). */
function when(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  if (d.toDateString() === new Date().toDateString()) return time
  return `${d.toLocaleDateString([], { day: '2-digit', month: '2-digit' })} ${time}`
}
</script>

<template>
  <Card body-class="flex h-full flex-col gap-2.5 p-4" :aria-label="t('dashboard.stat.today.title')">
    <div class="flex items-center gap-2 text-xs font-semibold text-muted-foreground">
      <span class="grid h-6 w-6 place-items-center rounded-md bg-muted text-muted-foreground">
        <CircleCheckBig class="h-3.5 w-3.5" :stroke-width="2" aria-hidden="true" />
      </span>
      <span class="min-w-0 truncate">{{ t('dashboard.stat.today.title') }}</span>
      <Button variant="ghost" size="xs" class="ml-auto shrink-0 text-primary" :title="t('dashboard.stat.today.allTitle')" @click="emit('all')">
        {{ t('dashboard.stat.today.all') }}
      </Button>
    </div>
    <p v-if="!today" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.today.noData') }}</p>
    <template v-else>
      <div class="flex min-w-0 items-baseline gap-2">
        <span class="shrink-0 text-3xl leading-none font-bold whitespace-nowrap tabular-nums">{{ today.finished }}</span>
        <span class="shrink-0 text-[13px] font-semibold whitespace-nowrap">{{ t('dashboard.stat.today.finished') }}</span>
        <span v-if="today.failed" class="shrink-0 whitespace-nowrap rounded-full bg-destructive/15 px-2 py-0.5 text-[10px] font-semibold text-destructive">{{ t('dashboard.stat.today.failed', { n: today.failed }) }}</span>
      </div>
      <p v-if="!today.recent.length" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.today.empty') }}</p>
      <ul v-else class="flex flex-col gap-0.5">
        <li v-for="r in today.recent" :key="r.runId">
          <button
            type="button"
            class="flex w-full min-w-0 cursor-pointer items-center gap-2 rounded px-1 py-0.5 text-left text-xs hover:bg-accent/50"
            @click="emit('open', r)"
          >
            <XCircle v-if="r.status === 'failed'" class="h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="1.75" aria-hidden="true" />
            <CheckCircle2 v-else class="h-3.5 w-3.5 shrink-0" :class="r.status === 'done' ? 'text-success' : 'text-muted-foreground'" :stroke-width="1.75" aria-hidden="true" />
            <span class="sr-only">{{ t(`dashboard.stat.today.status.${r.status}`) }}</span>
            <span class="min-w-0 flex-1 truncate">
              {{ r.title || t('dashboard.untitled') }}
              <span class="text-muted-foreground">· {{ r.status === 'failed' && r.error ? r.error : r.projectName }}</span>
            </span>
            <span class="shrink-0 font-mono text-[10px] text-muted-foreground">{{ when(r.finishedAt) }}</span>
          </button>
        </li>
      </ul>
    </template>
  </Card>
</template>
