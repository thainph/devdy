<script setup lang="ts">
// AI section of the todo drawer (spec §3.6 b, d): ✨ Analyze (cancellable) and
// 🚀 Start with AI; after an analysis, its estimate range and any suggested
// priority / deadline with "Apply" — which only fills the drawer's fields
// (`apply`), saved with the drawer like any edit (BR-09: AI never writes them
// itself). The stored plan (`ai_plan`) is shown as markdown and can be edited
// in place (saved right away).
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Pencil, Rocket, Sparkles, X } from 'lucide-vue-next'
import { Button, Textarea } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useMarkdown } from '@/lib/markdown'
import { useFocusStore } from '@/stores/focus'
import { useFocusAiStore } from '@/stores/focusAi'
import { formatEstimate, type AiApply, type FocusTodo } from '@/lib/focus'
import { useFocusAiActions } from './useFocusAiActions'
import { useFocusText } from './useFocusText'

const props = defineProps<{ todo: FocusTodo }>()
const emit = defineEmits<{ apply: [patch: AiApply] }>()

const { t } = useI18n()
const { toast } = useToast()
const store = useFocusStore()
const ai = useFocusAiStore()
const actions = useFocusAiActions()
const { stamp } = useFocusText()
const { renderText, loadMarkdown } = useMarkdown()
onMounted(loadMarkdown)

const running = computed(() => ai.isRunning(`todo:${props.todo.id}`))
const analysis = computed(() => ai.analyses[props.todo.id] ?? null)

const estimateText = computed(() => {
  const a = analysis.value
  if (!a) return ''
  return t('focus.ai.estimate', {
    low: formatEstimate(a.estimate_low_min),
    high: formatEstimate(a.estimate_high_min),
    likely: formatEstimate(a.estimate_min),
  })
})
const suggestedPriority = computed(() => {
  const p = analysis.value?.suggested_priority
  return p && p !== props.todo.priority ? p : null
})
const suggestedDue = computed(() => {
  const a = analysis.value
  if (!a?.suggested_due_date) return null
  if (a.suggested_due_date === props.todo.due_date && (a.suggested_due_time ?? null) === props.todo.due_time) return null
  return { date: a.suggested_due_date, time: a.suggested_due_time }
})

// ── Plan editing ──
const editing = ref(false)
const planDraft = ref('')
const savingPlan = ref(false)
watch(
  () => props.todo.id,
  () => (editing.value = false),
)
function editPlan() {
  planDraft.value = props.todo.ai_plan ?? ''
  editing.value = true
}
async function savePlan() {
  savingPlan.value = true
  try {
    await store.update(props.todo.id, { ai_plan: planDraft.value.trim() || null })
    editing.value = false
  } catch (e) {
    toast.error(String(e))
  } finally {
    savingPlan.value = false
  }
}
</script>

<template>
  <section class="space-y-2 rounded-md border border-border/60 bg-muted/10 p-3">
    <div class="flex flex-wrap items-center gap-2">
      <h4 class="mr-auto text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
        {{ t('focus.drawer.aiPlan') }}
        <span v-if="todo.ai_analyzed_at" class="ml-1 normal-case tracking-normal">
          · {{ t('focus.drawer.aiAnalyzedAt', { time: stamp(todo.ai_analyzed_at) }) }}
        </span>
      </h4>
      <template v-if="running">
        <span class="flex items-center gap-1 text-[11px] text-muted-foreground">
          <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          {{ t('focus.ai.analyzing') }}
        </span>
        <Button variant="ghost" size="xs" @click="actions.cancelAnalyze(todo)">
          <X class="h-3 w-3" :stroke-width="2" />
          {{ t('focus.ai.cancel') }}
        </Button>
      </template>
      <Button v-else variant="outline" size="xs" :disabled="todo.done" @click="actions.analyze(todo)">
        <Sparkles class="h-3 w-3" :stroke-width="1.75" />
        {{ todo.ai_plan ? t('focus.ai.reanalyze') : t('focus.ai.analyze') }}
      </Button>
      <Button variant="outline" size="xs" :disabled="todo.done" @click="actions.startWithAi(todo)">
        <Rocket class="h-3 w-3" :stroke-width="1.75" />
        {{ t('focus.ai.start') }}
      </Button>
    </div>

    <!-- What the last analysis proposes (applied into the form, saved with it) -->
    <div v-if="analysis" class="space-y-1 text-xs">
      <p class="flex flex-wrap items-center gap-2">
        <span>{{ estimateText }}</span>
        <Button variant="ghost" size="xs" class="text-primary" @click="emit('apply', { estimateMin: analysis.estimate_min })">
          {{ t('focus.ai.apply') }}
        </Button>
      </p>
      <p v-if="suggestedPriority" class="flex flex-wrap items-center gap-2">
        <span>{{ t('focus.ai.suggestedPriority', { p: suggestedPriority }) }}</span>
        <Button variant="ghost" size="xs" class="text-primary" @click="emit('apply', { priority: suggestedPriority })">
          {{ t('focus.ai.apply') }}
        </Button>
      </p>
      <p v-if="suggestedDue" class="flex flex-wrap items-center gap-2">
        <span>{{ t('focus.ai.suggestedDue', { due: suggestedDue.time ? `${suggestedDue.date} ${suggestedDue.time}` : suggestedDue.date }) }}</span>
        <Button
          variant="ghost"
          size="xs"
          class="text-primary"
          @click="emit('apply', { dueDate: suggestedDue.date, dueTime: suggestedDue.time ?? '' })"
        >
          {{ t('focus.ai.apply') }}
        </Button>
      </p>
    </div>

    <!-- The stored plan: markdown, editable -->
    <template v-if="editing">
      <Textarea v-model="planDraft" class="min-h-[10rem] w-full font-mono text-xs" />
      <div class="flex justify-end gap-2">
        <Button variant="ghost" size="xs" @click="editing = false">{{ t('focus.drawer.cancel') }}</Button>
        <Button size="xs" :disabled="savingPlan" @click="savePlan">{{ t('focus.drawer.save') }}</Button>
      </div>
    </template>
    <div v-else-if="todo.ai_plan" class="group relative">
      <div class="markdown-output text-sm" v-html="renderText(todo.ai_plan)" />
      <Button
        variant="ghost"
        size="icon-sm"
        class="absolute right-0 top-0 opacity-0 transition-opacity group-hover:opacity-100"
        :title="t('focus.ai.editPlan')"
        :aria-label="t('focus.ai.editPlan')"
        @click="editPlan"
      >
        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </div>
    <p v-else-if="!running" class="text-[11px] text-muted-foreground">{{ t('focus.ai.noPlan') }}</p>
  </section>
</template>
