<script setup lang="ts">
// Detail drawer of one watched source (§6.1): health + status, summary, next
// action, agreed deadlines / decisions / open questions each with the quoted
// evidence (click → open the source), when it was summarised and checked, the
// last error, and the actions — open source, refresh it, move to another
// project, unwatch.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { ExternalLink, Github, RefreshCw, Slack, TriangleAlert } from 'lucide-vue-next'
import { Badge, Button, Drawer } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { HEALTH_DOT, type PulseWatchItem } from '@/lib/pulse'
import { usePulseText } from './usePulseText'

const props = defineProps<{ watch: PulseWatchItem | null; refreshing: boolean }>()
const emit = defineEmits<{ close: []; refresh: []; move: [projectId: string]; unwatch: [] }>()

const { t } = useI18n()
const { ballText, dueText, errorText, stamp } = usePulseText()

const d = computed(() => props.watch?.digest ?? null)
const title = computed(() => props.watch?.title || props.watch?.sourceRef || '')

const STATUS_TONE = {
  on_track: 'success',
  at_risk: 'warning',
  blocked: 'error',
  waiting: 'info',
  done: 'neutral',
} as const

function openSource() {
  if (props.watch) void openUrl(props.watch.sourceUrl).catch(() => {})
}

function onMove(pid: string | null) {
  if (pid && props.watch && pid !== props.watch.projectId) emit('move', pid)
}
</script>

<template>
  <Drawer :open="!!watch" size="lg" @close="emit('close')">
    <template #header>
      <div v-if="watch" class="flex min-w-0 flex-1 items-center gap-2">
        <span
          class="h-2.5 w-2.5 shrink-0 rounded-full"
          :class="HEALTH_DOT[watch.health]"
          role="img"
          :aria-label="t(`pulse.health.${watch.health}`)"
          :title="t(`pulse.health.${watch.health}`)"
        />
        <component
          :is="watch.sourceType === 'slack' ? Slack : Github"
          class="h-4 w-4 shrink-0 text-muted-foreground"
          :stroke-width="1.75"
        />
        <h3 class="min-w-0 flex-1 truncate text-sm font-semibold" :title="title">{{ title }}</h3>
      </div>
    </template>

    <div v-if="watch" class="space-y-5 p-4 text-sm">
      <!-- Health / status / ball -->
      <div class="flex flex-wrap items-center gap-1.5">
        <Badge size="sm" :tone="watch.health === 'red' ? 'error' : watch.health === 'yellow' ? 'warning' : watch.health === 'green' ? 'success' : 'neutral'">
          {{ t(`pulse.health.${watch.health}`) }}
        </Badge>
        <Badge v-if="d" size="sm" :tone="STATUS_TONE[d.status]">{{ t(`pulse.status.${d.status}`) }}</Badge>
        <span v-if="ballText(d)" class="text-xs" :class="watch.needsMe ? 'font-medium text-red-600 dark:text-red-400' : 'text-muted-foreground'">
          {{ ballText(d) }}
        </span>
        <span class="ml-auto truncate text-[11px] text-muted-foreground">{{ watch.projectName }} · {{ watch.sourceRef }}</span>
      </div>

      <div
        v-if="watch.lastError"
        class="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300"
      >
        <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span>{{ t('pulse.drawer.lastError') }}: {{ errorText(watch.lastError) }}</span>
      </div>

      <p v-if="!d" class="text-xs text-muted-foreground">{{ t('pulse.drawer.notSummarized') }}</p>

      <template v-if="d">
        <section class="space-y-1">
          <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('pulse.drawer.summary') }}</h4>
          <p class="leading-relaxed">{{ d.summary }}</p>
          <p v-if="d.truncated" class="text-[11px] text-muted-foreground">{{ t('pulse.drawer.truncated') }}</p>
        </section>

        <section v-if="d.nextAction" class="space-y-1">
          <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('pulse.drawer.nextAction') }}</h4>
          <p class="leading-relaxed">{{ d.nextAction }}</p>
        </section>

        <section class="space-y-1.5">
          <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('pulse.drawer.deadlines') }}</h4>
          <p v-if="!d.deadlines.length" class="text-xs text-muted-foreground">{{ t('pulse.drawer.none') }}</p>
          <div v-for="(x, i) in d.deadlines" :key="`dl-${i}`" class="rounded-md border border-border/60 bg-muted/20 px-3 py-2">
            <p class="text-xs font-medium">{{ x.title }} — {{ dueText({ dueAt: x.due_at, allDay: x.all_day }) }}</p>
            <button
              type="button"
              class="mt-1 block text-left text-[11px] italic text-muted-foreground hover:text-foreground cursor-pointer"
              :title="t('pulse.drawer.evidenceHint')"
              @click="openSource"
            >“{{ x.evidence }}”</button>
          </div>
        </section>

        <section class="space-y-1.5">
          <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('pulse.drawer.decisions') }}</h4>
          <p v-if="!d.decisions.length" class="text-xs text-muted-foreground">{{ t('pulse.drawer.none') }}</p>
          <div v-for="(x, i) in d.decisions" :key="`dc-${i}`" class="rounded-md border border-border/60 bg-muted/20 px-3 py-2">
            <p class="text-xs">
              <span v-if="x.decided_at" class="mr-1 text-muted-foreground">{{ x.decided_at }}</span>{{ x.text }}
            </p>
            <button
              type="button"
              class="mt-1 block text-left text-[11px] italic text-muted-foreground hover:text-foreground cursor-pointer"
              :title="t('pulse.drawer.evidenceHint')"
              @click="openSource"
            >“{{ x.evidence }}”</button>
          </div>
        </section>

        <section class="space-y-1.5">
          <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('pulse.drawer.openQuestions') }}</h4>
          <p v-if="!d.openQuestions.length" class="text-xs text-muted-foreground">{{ t('pulse.drawer.none') }}</p>
          <div v-for="(x, i) in d.openQuestions" :key="`q-${i}`" class="rounded-md border border-border/60 bg-muted/20 px-3 py-2">
            <p class="text-xs">
              <span class="mr-1 text-muted-foreground">{{ x.asked_by === 'me' ? t('pulse.drawer.askedByMe') : t('pulse.drawer.askedByOther') }}:</span>{{ x.text }}
            </p>
            <button
              v-if="x.evidence"
              type="button"
              class="mt-1 block text-left text-[11px] italic text-muted-foreground hover:text-foreground cursor-pointer"
              :title="t('pulse.drawer.evidenceHint')"
              @click="openSource"
            >“{{ x.evidence }}”</button>
          </div>
        </section>
      </template>

      <!-- Move to another project -->
      <section class="space-y-1.5">
        <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('pulse.drawer.moveProject') }}</h4>
        <div class="w-60">
          <ProjectSelect :model-value="watch.projectId" variant="default" size="sm" @update:model-value="onMove" />
        </div>
      </section>

      <div class="space-y-0.5 text-[11px] text-muted-foreground">
        <p v-if="d">{{ t('pulse.drawer.summarizedAt', { time: stamp(d.createdAt) }) }}<span v-if="d.model"> · {{ t('pulse.drawer.model') }}: {{ d.model }}</span></p>
        <p v-if="watch.checkedAt">{{ t('pulse.drawer.checkedAt', { time: stamp(watch.checkedAt) }) }}</p>
      </div>
    </div>

    <template #footer>
      <Button variant="outline" size="sm" @click="openSource">
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('pulse.drawer.openSource') }}
      </Button>
      <Button variant="outline" size="sm" :disabled="refreshing" @click="emit('refresh')">
        <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': refreshing }" :stroke-width="1.75" />
        {{ t('pulse.drawer.refreshSource') }}
      </Button>
      <Button variant="destructive" size="sm" class="ml-auto" @click="emit('unwatch')">
        {{ t('pulse.drawer.unwatch') }}
      </Button>
    </template>
  </Drawer>
</template>
