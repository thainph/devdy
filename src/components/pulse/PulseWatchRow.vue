<script setup lang="ts">
// One watched source in a project card (§6.1): health dot · source icon ·
// title, then the one-line summary (full text in the tooltip), who holds the
// ball and for how long, the nearest deadline and an error chip. Click → the
// detail drawer.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Github, Loader2, Slack, TriangleAlert } from 'lucide-vue-next'
import { HEALTH_DOT, type PulseWatchItem } from '@/lib/pulse'
import { usePulseText } from './usePulseText'

const props = defineProps<{ watch: PulseWatchItem }>()
const emit = defineEmits<{ open: [] }>()

const { t } = useI18n()
const { ballText, dueText, errorText } = usePulseText()

const title = computed(() => props.watch.title || props.watch.sourceRef)
const ball = computed(() => ballText(props.watch.digest))
const resolving = computed(() => props.watch.resolveStatus === 'pending')
</script>

<template>
  <button
    type="button"
    class="group flex w-full min-w-0 items-start gap-2 rounded-md px-2 py-1.5 text-left transition-colors cursor-pointer hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
    @click="emit('open')"
  >
    <span
      class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
      :class="HEALTH_DOT[watch.health]"
      role="img"
      :aria-label="t(`pulse.health.${watch.health}`)"
      :title="t(`pulse.health.${watch.health}`)"
    />
    <component
      :is="watch.sourceType === 'slack' ? Slack : Github"
      class="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground"
      :stroke-width="1.75"
      :aria-label="watch.sourceType === 'slack' ? t('pulse.row.slack') : t('pulse.row.github')"
    />
    <span class="min-w-0 flex-1">
      <span class="flex min-w-0 items-center gap-1.5">
        <span class="truncate text-xs font-medium text-foreground">{{ title }}</span>
        <span
          v-if="watch.lastError"
          class="inline-flex shrink-0 items-center gap-0.5 rounded border border-amber-500/40 bg-amber-500/10 px-1 text-[10px] font-medium text-amber-600 dark:text-amber-400"
          :title="errorText(watch.lastError)"
        >
          <TriangleAlert class="h-2.5 w-2.5" :stroke-width="2" />
          {{ t('pulse.row.error') }}
        </span>
        <Loader2 v-if="resolving" class="h-3 w-3 shrink-0 animate-spin text-muted-foreground" :stroke-width="1.75" />
      </span>
      <span
        class="block truncate text-[11px] text-muted-foreground"
        :title="watch.digest?.summary || ''"
      >
        {{ watch.digest?.summary || (resolving ? t('pulse.row.resolving') : t('pulse.row.noDigest')) }}
      </span>
      <span v-if="ball || watch.nextDue" class="mt-0.5 flex flex-wrap items-center gap-x-2 text-[10px]">
        <span
          v-if="ball"
          :class="watch.needsMe ? 'font-medium text-red-600 dark:text-red-400' : 'text-muted-foreground'"
        >{{ ball }}</span>
        <span v-if="watch.nextDue" class="text-muted-foreground" :class="{ 'text-red-600 dark:text-red-400': watch.overdue }">
          {{ watch.nextDue.title }} — {{ dueText(watch.nextDue) }}
        </span>
      </span>
    </span>
  </button>
</template>
