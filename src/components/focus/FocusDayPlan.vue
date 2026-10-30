<script setup lang="ts">
// "Plan today ✨" (spec §3.6c): the header button and the timeline it opens.
// The AI fits today's open todos into the free working time (blocks the backend
// could not validate are dropped and counted), lists what it deferred, and the
// plan is kept per day (`focus_day_plans`, re-planning overwrites it). Blocks
// can be pushed to Google Calendar one by one or all at once as
// `[Devdy] <todo>` events. Clicking a block opens its todo.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { CalendarCheck, CalendarPlus, Loader2, RefreshCw, Sparkles, X } from 'lucide-vue-next'
import { Button, Drawer } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { useFocusAiStore } from '@/stores/focusAi'
import { firstLine } from '@/lib/itemText'
import { blockMinutes, formatEstimate, ymdOf } from '@/lib/focus'
import { useFocusAiActions } from './useFocusAiActions'
import { useFocusText } from './useFocusText'

const emit = defineEmits<{ open: [todoId: string] }>()

const { t } = useI18n()
const { toast } = useToast()
const store = useFocusStore()
const ai = useFocusAiStore()
const { fail } = useFocusAiActions()
const { stamp } = useFocusText()

const open = ref(false)
const pushing = ref<string | 'all' | null>(null)

const planning = computed(() => ai.isRunning('plan'))
/** Only today's plan is shown; an older stored one is stale. */
const plan = computed(() => (ai.plan && ai.plan.date === ymdOf(Date.now()) ? ai.plan : null))
const pending = computed(() => plan.value?.blocks.filter((b) => !b.calendar_event_id) ?? [])

function title(todoId: string): string {
  const it = store.items.find((i) => i.todo.id === todoId)
  return it ? firstLine(it.todo.text) || it.todo.text : t('focus.plan.missingTodo')
}

onMounted(() => void ai.loadPlan().catch(() => {}))

async function makePlan() {
  open.value = true
  try {
    await ai.planToday()
  } catch (e) {
    fail(e)
  }
}

async function push(blockId?: string) {
  pushing.value = blockId ?? 'all'
  try {
    await ai.planToCalendar(blockId ? [blockId] : undefined)
    toast.success(t('focus.plan.pushed'))
  } catch (e) {
    fail(e)
  } finally {
    pushing.value = null
  }
}
</script>

<template>
  <Button
    variant="outline"
    size="sm"
    :title="t('focus.plan.hint')"
    @click="plan || planning ? (open = true) : makePlan()"
  >
    <Loader2 v-if="planning" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
    <Sparkles v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
    {{ t('focus.plan.button') }}
  </Button>

  <Drawer :open="open" side="left" size="md" :title="t('focus.plan.title')" @close="open = false">
    <div class="space-y-4 p-4 text-sm">
      <!-- Running -->
      <div v-if="planning" class="flex items-center gap-2 text-xs text-muted-foreground">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" />
        <span class="flex-1">{{ t('focus.plan.planning') }}</span>
        <Button variant="ghost" size="xs" @click="ai.cancel('plan')">
          <X class="h-3 w-3" :stroke-width="2" />
          {{ t('focus.ai.cancel') }}
        </Button>
      </div>

      <p v-else-if="!plan" class="text-xs text-muted-foreground">{{ t('focus.plan.empty') }}</p>

      <template v-if="plan">
        <p v-if="plan.summary" class="leading-relaxed">{{ plan.summary }}</p>

        <!-- Timeline -->
        <ol class="relative space-y-2 border-l border-border pl-4">
          <li v-for="b in plan.blocks" :key="b.id" class="relative">
            <span class="absolute -left-[1.3rem] top-1.5 h-2 w-2 rounded-full" :class="b.calendar_event_id ? 'bg-emerald-500' : 'bg-primary'" />
            <div class="flex items-start gap-2">
              <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" @click="emit('open', b.todo_id)">
                <p class="text-xs font-semibold tabular-nums text-muted-foreground">
                  {{ b.start }}–{{ b.end }} · {{ formatEstimate(blockMinutes(b)) }}
                </p>
                <p class="truncate font-medium hover:underline">{{ title(b.todo_id) }}</p>
                <p v-if="b.note" class="text-xs text-muted-foreground">{{ b.note }}</p>
              </button>
              <span v-if="b.calendar_event_id" class="shrink-0 text-emerald-600 dark:text-emerald-400" :title="t('focus.plan.onCalendar')">
                <CalendarCheck class="h-4 w-4" :stroke-width="1.75" />
              </span>
              <Button
                v-else
                variant="ghost"
                size="icon-sm"
                :disabled="!!pushing"
                :title="t('focus.plan.pushOne')"
                :aria-label="t('focus.plan.pushOne')"
                @click="push(b.id)"
              >
                <Loader2 v-if="pushing === b.id" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
                <CalendarPlus v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </div>
          </li>
        </ol>
        <p v-if="!plan.blocks.length" class="text-xs text-muted-foreground">{{ t('focus.plan.noBlocks') }}</p>
        <p v-if="plan.dropped" class="text-[11px] text-muted-foreground">{{ t('focus.plan.dropped', { n: plan.dropped }, plan.dropped) }}</p>

        <section v-if="plan.deferred.length" class="space-y-1">
          <h4 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('focus.plan.deferred') }}</h4>
          <button
            v-for="d in plan.deferred"
            :key="d.todo_id"
            type="button"
            class="block w-full cursor-pointer rounded px-1 py-0.5 text-left text-xs hover:bg-accent/50"
            @click="emit('open', d.todo_id)"
          >
            <span class="font-medium">{{ title(d.todo_id) }}</span>
            <span v-if="d.reason" class="text-muted-foreground"> — {{ d.reason }}</span>
          </button>
        </section>

        <p class="text-[11px] text-muted-foreground">{{ t('focus.plan.createdAt', { time: stamp(plan.created_at) }) }}</p>
      </template>
    </div>

    <template #footer>
      <Button v-if="plan && pending.length" variant="outline" size="sm" :disabled="!!pushing || planning" @click="push()">
        <Loader2 v-if="pushing === 'all'" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
        <CalendarPlus v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('focus.plan.pushAll') }}
      </Button>
      <span class="flex-1" />
      <Button size="sm" :disabled="planning" @click="makePlan">
        <RefreshCw v-if="plan" class="h-3.5 w-3.5" :stroke-width="1.75" />
        <Sparkles v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ plan ? t('focus.plan.replan') : t('focus.plan.make') }}
      </Button>
    </template>
  </Drawer>
</template>
