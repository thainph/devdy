<script setup lang="ts">
/**
 * "Plan today ✨" on the phone (spec §3.6c, mirrors the desktop's
 * FocusDayPlan): a full-height screen with today's saved plan (asked with
 * `focus_day_plan_get` on open) — the timeline of blocks with their todos,
 * what was deferred, the summary — and the AI planning it (spinner + Cancel,
 * re-planning overwrites it). Blocks go to Google Calendar one by one or all
 * at once; tapping a block or a deferred todo opens it in the editor.
 */
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { CalendarCheck, CalendarPlus, ChevronLeft, Loader2, RefreshCw, Sparkles, X } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import { blockMinutes, formatEstimate, type DayPlan, type FocusItem } from '@/lib/focus'
import { pendingBlocks } from '@/lib/remoteFocus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'
import ControllerHeader from './ControllerHeader.vue'
import HeaderButton from './HeaderButton.vue'
import type { WriteOutcome } from '../dataStore'

const props = defineProps<{
  /** The last `focus_day_plan` (null until it arrives). */
  dayPlan: { date: string; plan: DayPlan | null } | null
  /** The plan request failed. */
  failed: boolean
  /** The AI is planning. */
  planning: boolean
  /** The last planning failure, translated. */
  planError: string | null
  /** The board's rows, for the todo titles. */
  items: FocusItem[]
  /** The last refused write, translated. */
  error: string | null
  push: (blockIds?: string[]) => Promise<WriteOutcome>
}>()

const emit = defineEmits<{
  back: []
  load: []
  make: []
  cancel: []
  open: [todoId: string]
  'clear-error': []
}>()

const { t } = useI18n()
const { stamp } = useFocusText()

onMounted(() => emit('load'))

const loading = computed(() => !props.dayPlan && !props.failed)
// The Host's today (asked without a date on open, so a phone in another time
// zone still sees the desktop's day), or the plan just made.
const plan = computed(() => props.dayPlan?.plan ?? null)
const pending = computed(() => pendingBlocks(plan.value))
const pushing = ref<string | 'all' | null>(null)

function title(todoId: string): string {
  const it = props.items.find((i) => i.todo.id === todoId)
  return it ? splitTodoText(it.todo.text).title || it.todo.text : t('focus.plan.missingTodo')
}

async function onPush(blockId?: string): Promise<void> {
  if (pushing.value) return
  pushing.value = blockId ?? 'all'
  try {
    await props.push(blockId ? [blockId] : undefined)
  } finally {
    pushing.value = null
  }
}

const banner = computed(() => props.planError ?? props.error)
const heading = 'mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
</script>

<template>
  <div class="app-shell relative flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="t('focus.plan.title')">
      <template #leading>
        <HeaderButton class="-ml-1.5" :label="t('controller.data.back')" @click="emit('back')">
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </HeaderButton>
      </template>
    </ControllerHeader>

    <Banner
      v-if="banner"
      tone="destructive"
      class="shrink-0 gap-2 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{ banner }}
      <template #actions>
        <button
          class="shrink-0 cursor-pointer text-destructive"
          :title="t('controller.data.dismiss')"
          :aria-label="t('controller.data.dismiss')"
          @click="emit('clear-error')"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </template>
    </Banner>

    <main class="flex-1 min-h-0 overflow-auto px-3 py-3">
      <div v-if="planning" class="mb-3 flex items-center gap-2 rounded-xl border border-border bg-card px-3 py-2.5 text-xs text-muted-foreground">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" />
        <span class="flex-1">{{ t('focus.plan.planning') }}</span>
        <Button variant="ghost" size="xs" @click="emit('cancel')">
          <X class="h-3 w-3" :stroke-width="2" />
          {{ t('focus.ai.cancel') }}
        </Button>
      </div>

      <p v-if="loading && !planning" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>
      <div v-else-if="failed && !plan" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <Button variant="outline" @click="emit('load')">{{ t('controller.data.retry') }}</Button>
      </div>
      <div v-else-if="!plan && !planning" class="flex flex-col items-center gap-3 py-10 text-center text-sm text-foreground/50">
        <p>{{ t('focus.plan.empty') }}</p>
        <p class="max-w-72 text-xs">{{ t('focus.plan.hint') }}</p>
      </div>

      <template v-if="plan">
        <p v-if="plan.summary" class="mb-3 text-sm leading-relaxed">{{ plan.summary }}</p>

        <div v-if="plan.blocks.length" class="ui-card mb-3 overflow-hidden rounded-xl border border-border bg-card">
          <div
            v-for="b in plan.blocks"
            :key="b.id"
            class="flex items-start gap-2.5 border-b border-border/60 px-3 py-2.5 last:border-b-0"
          >
            <span class="mt-1.5 h-2 w-2 shrink-0 rounded-full" :class="b.calendar_event_id ? 'bg-success' : 'bg-primary'" />
            <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" @click="emit('open', b.todo_id)">
              <p class="text-xs font-semibold tabular-nums text-muted-foreground">
                {{ b.start }}–{{ b.end }} · {{ formatEstimate(blockMinutes(b)) }}
              </p>
              <p class="text-sm font-medium break-words">{{ title(b.todo_id) }}</p>
              <p v-if="b.note" class="mt-0.5 text-xs text-muted-foreground">{{ b.note }}</p>
            </button>
            <span v-if="b.calendar_event_id" class="mt-0.5 shrink-0 text-success" :title="t('focus.plan.onCalendar')">
              <CalendarCheck class="h-4 w-4" :stroke-width="2" />
            </span>
            <button
              v-else
              type="button"
              class="-mr-1 grid h-8 w-8 shrink-0 place-items-center rounded-md text-muted-foreground cursor-pointer hover:bg-accent disabled:opacity-50"
              :disabled="!!pushing || planning"
              :title="t('focus.plan.pushOne')"
              :aria-label="t('focus.plan.pushOne')"
              @click="onPush(b.id)"
            >
              <Loader2 v-if="pushing === b.id" class="h-4 w-4 animate-spin" :stroke-width="2" />
              <CalendarPlus v-else class="h-4 w-4" :stroke-width="2" />
            </button>
          </div>
        </div>
        <p v-else class="mb-3 text-xs text-muted-foreground">{{ t('focus.plan.noBlocks') }}</p>
        <p v-if="plan.dropped" class="mb-3 text-[11px] text-muted-foreground">
          {{ t('focus.plan.dropped', { n: plan.dropped }, plan.dropped) }}
        </p>

        <section v-if="plan.deferred.length" class="mb-3">
          <h2 :class="heading">{{ t('focus.plan.deferred') }}</h2>
          <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
            <button
              v-for="d in plan.deferred"
              :key="d.todo_id"
              type="button"
              class="block w-full cursor-pointer border-b border-border/60 px-3 py-2.5 text-left text-sm last:border-b-0"
              @click="emit('open', d.todo_id)"
            >
              <span class="font-medium">{{ title(d.todo_id) }}</span>
              <span v-if="d.reason" class="block text-xs text-muted-foreground">{{ d.reason }}</span>
            </button>
          </div>
        </section>

        <p class="text-[11px] text-muted-foreground">{{ t('focus.plan.createdAt', { time: stamp(plan.created_at) }) }}</p>
      </template>
    </main>

    <div class="grid shrink-0 grid-cols-2 gap-2 border-t border-border px-3 pt-2.5 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
      <Button
        variant="outline"
        size="md"
        :disabled="!plan || !pending.length || !!pushing || planning"
        @click="onPush()"
      >
        <Loader2 v-if="pushing === 'all'" class="h-4 w-4 animate-spin" :stroke-width="2" />
        <CalendarPlus v-else class="h-4 w-4" :stroke-width="2" />
        {{ t('focus.plan.pushAll') }}
      </Button>
      <Button size="md" :disabled="planning" @click="emit('make')">
        <RefreshCw v-if="plan" class="h-4 w-4" :stroke-width="2" />
        <Sparkles v-else class="h-4 w-4" :stroke-width="2" />
        {{ plan ? t('focus.plan.replan') : t('focus.plan.make') }}
      </Button>
    </div>
  </div>
</template>
