<script setup lang="ts">
// A project card on the Pulse screen (§6.1): health dot + name + refresh, the
// next milestone with its distance, count chips (needs me · waiting on customer
// · overdue · watching), up to 5 open sources ("+N more" expands) and the
// collapsed "Done" group (BR-12).
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ChevronRight, RefreshCw } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { HEALTH_DOT, splitRows, type ProjectPulse, type PulseWatchItem } from '@/lib/pulse'
import { usePulseText } from './usePulseText'
import PulseWatchRow from './PulseWatchRow.vue'

const props = defineProps<{ project: ProjectPulse; refreshing: boolean }>()
const emit = defineEmits<{ refresh: []; open: [watch: PulseWatchItem] }>()

const { t } = useI18n()
const { dueText } = usePulseText()

const expanded = ref(false)
const showFinished = ref(false)

const rows = computed(() => splitRows(props.project.watches, expanded.value ? Infinity : undefined))

const chips = computed(() => {
  const c = props.project.counts
  return [
    { key: 'needsMe', n: c.needsMe, tone: 'text-red-600 dark:text-red-400 border-red-500/30 bg-red-500/10' },
    { key: 'waitingCustomer', n: c.waitingCustomer, tone: 'text-amber-600 dark:text-amber-400 border-amber-500/30 bg-amber-500/10' },
    { key: 'overdue', n: c.overdue, tone: 'text-red-600 dark:text-red-400 border-red-500/30 bg-red-500/10' },
    { key: 'watching', n: c.watching, tone: 'text-muted-foreground border-border/60 bg-muted/40' },
  ].filter((x) => x.key === 'watching' || x.n > 0)
})
</script>

<template>
  <Card class="flex flex-col border-border/60" body-class="flex flex-col flex-1 gap-2.5 p-4">
    <!-- Name + refresh -->
    <div class="flex items-center gap-2">
      <span
        class="h-2.5 w-2.5 shrink-0 rounded-full"
        :class="HEALTH_DOT[project.health]"
        role="img"
        :aria-label="t(`pulse.health.${project.health}`)"
        :title="t(`pulse.health.${project.health}`)"
      />
      <h2 class="min-w-0 flex-1 truncate text-sm font-semibold">{{ project.projectName }}</h2>
      <Button
        variant="ghost"
        size="icon-sm"
        :disabled="refreshing || project.watches.length === 0"
        :title="t('pulse.card.refresh')"
        :aria-label="t('pulse.card.refresh')"
        @click="emit('refresh')"
      >
        <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': refreshing }" :stroke-width="1.75" />
      </Button>
    </div>

    <!-- Next milestone -->
    <p class="truncate text-xs" :class="project.nextMilestone ? 'text-foreground' : 'text-muted-foreground'">
      <template v-if="project.nextMilestone">
        {{ t('pulse.card.nextMilestone', {
          title: project.nextMilestone.title || t('pulse.card.untitled'),
          date: dueText(project.nextMilestone),
        }) }}
      </template>
      <template v-else>{{ t('pulse.card.noMilestone') }}</template>
    </p>

    <!-- Count chips -->
    <div class="flex flex-wrap gap-1.5">
      <span
        v-for="c in chips"
        :key="c.key"
        class="rounded-full border px-2 py-0.5 text-[10px] font-medium"
        :class="c.tone"
      >
        {{ t(`pulse.counts.${c.key}`, { n: c.n }) }}
      </span>
    </div>

    <!-- Open sources -->
    <div v-if="rows.shown.length" class="-mx-2 flex flex-col">
      <PulseWatchRow v-for="w in rows.shown" :key="w.id" :watch="w" @open="emit('open', w)" />
      <button
        v-if="rows.more > 0"
        type="button"
        class="mx-2 mt-0.5 self-start text-[11px] text-muted-foreground hover:text-foreground cursor-pointer"
        @click="expanded = true"
      >
        {{ t('pulse.card.more', { n: rows.more }) }}
      </button>
    </div>
    <p v-else-if="!rows.finished.length" class="text-xs text-muted-foreground">{{ t('pulse.card.noWatches') }}</p>

    <!-- Done group (collapsed) -->
    <div v-if="rows.finished.length" class="border-t border-border/50 pt-2">
      <button
        type="button"
        class="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground cursor-pointer"
        :aria-expanded="showFinished"
        @click="showFinished = !showFinished"
      >
        <component :is="showFinished ? ChevronDown : ChevronRight" class="h-3 w-3" :stroke-width="1.75" />
        {{ t('pulse.card.finished', { n: rows.finished.length }) }}
      </button>
      <div v-if="showFinished" class="-mx-2 mt-1 flex flex-col opacity-80">
        <PulseWatchRow v-for="w in rows.finished" :key="w.id" :watch="w" @open="emit('open', w)" />
      </div>
    </div>
  </Card>
</template>
