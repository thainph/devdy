<script setup lang="ts">
// Master column of the Pulse screen: search + "needs attention" filter and one
// compact row per project (health dot, name, next milestone, needs-me count),
// styled like the Slack threads sidebar. Selecting a row shows its detail.
import { useI18n } from 'vue-i18n'
import { Search } from 'lucide-vue-next'
import { Input } from '@/components/ui'
import { HEALTH_DOT, type ProjectPulse } from '@/lib/pulse'
import { usePulseText } from './usePulseText'

defineProps<{ projects: ProjectPulse[]; selectedId: string | null; total: number }>()
const emit = defineEmits<{ select: [projectId: string] }>()
const query = defineModel<string>('query', { required: true })
const attentionOnly = defineModel<boolean>('attentionOnly', { required: true })

const { t } = useI18n()
const { dueText } = usePulseText()
</script>

<template>
  <aside class="flex w-[260px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <div class="space-y-2 p-3 border-b border-border/60">
      <div class="relative">
        <Search class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
        <Input v-model="query" class="pl-8" :placeholder="t('pulse.search')" />
      </div>
      <button
        type="button"
        role="switch"
        :aria-checked="attentionOnly"
        class="flex items-center gap-2 text-xs cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-md px-1 py-1"
        @click="attentionOnly = !attentionOnly"
      >
        <span
          class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
          :class="attentionOnly ? 'bg-primary' : 'bg-muted'"
        >
          <span
            class="inline-block h-4 w-4 rounded-full bg-white shadow transition-transform"
            :class="attentionOnly ? 'translate-x-4' : 'translate-x-0.5'"
          />
        </span>
        <span :class="attentionOnly ? 'text-foreground' : 'text-muted-foreground'">{{ t('pulse.attentionOnly') }}</span>
      </button>
    </div>

    <ul class="flex-1 overflow-y-auto p-2 space-y-px">
      <li v-for="p in projects" :key="p.projectId">
        <button
          type="button"
          class="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors"
          :class="p.projectId === selectedId ? 'bg-primary/15 text-foreground' : 'text-foreground/85 hover:bg-accent/50'"
          :aria-current="p.projectId === selectedId ? 'true' : undefined"
          @click="emit('select', p.projectId)"
        >
          <span
            class="h-2 w-2 shrink-0 rounded-full"
            :class="HEALTH_DOT[p.health]"
            role="img"
            :aria-label="t(`pulse.health.${p.health}`)"
            :title="t(`pulse.health.${p.health}`)"
          />
          <span class="min-w-0 flex-1">
            <span class="block truncate text-[13px] font-medium">{{ p.projectName }}</span>
            <span class="block truncate text-[11px] text-muted-foreground">
              {{ p.nextMilestone ? dueText(p.nextMilestone) : t('pulse.card.noMilestone') }}
            </span>
          </span>
          <span
            v-if="p.counts.needsMe > 0"
            class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-red-500/15 px-1.5 text-[10px] font-semibold text-red-600 dark:text-red-400"
            :title="t('pulse.counts.needsMe', { n: p.counts.needsMe })"
          >
            {{ p.counts.needsMe }}
          </span>
        </button>
      </li>
      <li v-if="projects.length === 0 && total > 0" class="px-2 py-6 text-center text-xs text-muted-foreground">
        {{ t('pulse.empty.filtered') }}
      </li>
    </ul>
  </aside>
</template>
