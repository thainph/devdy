<script setup lang="ts">
// "✨ Suggest" of the "+ Todo" form (spec §3.1 step 4, §3.6a): the AI reads the
// source and proposes title, deadline (only one quoted from the source),
// priority and estimate — filled into the form; nothing is saved until the user
// presses Add. Cancellable while it runs.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Sparkles, X } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useFocusAiStore } from '@/stores/focusAi'
import { applySuggestion, type TodoDraft } from '@/lib/focus'
import { useFocusAiActions } from './useFocusAiActions'

const draft = defineModel<TodoDraft>({ required: true })

const { t } = useI18n()
const { toast } = useToast()
const ai = useFocusAiStore()
const { fail } = useFocusAiActions()

const key = computed(() => `suggest:${draft.value.sourceUrl.trim()}`)
const running = computed(() => ai.isRunning(key.value))
const reason = ref('')

async function suggest() {
  const url = draft.value.sourceUrl.trim()
  if (!url) return
  try {
    const s = await ai.suggest(url, draft.value.projectId || null, draft.value.text.trim() || null)
    // The user may have typed meanwhile: apply onto the current draft.
    draft.value = applySuggestion(draft.value, s)
    reason.value = s.reason
    toast.success(s.reason ? t('focus.ai.suggestedWhy', { reason: s.reason }) : t('focus.ai.suggested'))
  } catch (e) {
    fail(e)
  }
}
</script>

<template>
  <span v-if="draft.sourceUrl" class="ml-auto flex items-center gap-1">
    <template v-if="running">
      <span class="flex items-center gap-1 text-[11px] text-muted-foreground">
        <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
        {{ t('focus.ai.suggesting') }}
      </span>
      <Button variant="ghost" size="xs" @click="ai.cancel(key)">
        <X class="h-3 w-3" :stroke-width="2" />
        {{ t('focus.ai.cancel') }}
      </Button>
    </template>
    <Button v-else variant="outline" size="xs" :title="reason || t('focus.ai.suggestHint')" @click="suggest">
      <Sparkles class="h-3 w-3" :stroke-width="1.75" />
      {{ t('focus.capture.suggest') }}
    </Button>
  </span>
</template>
