<script setup lang="ts">
// The two follow-up dialogs of the board's actions (useFocusActions), mounted
// once by FocusView: "How long did it take?" after Done on a started, estimated
// todo (§3.3, skippable), and "Snooze until…" with a picked date-time.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Input, Modal } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { formatEstimate, fromLocalInput, parseEstimate, toLocalInput } from '@/lib/focus'
import { useFocusActions } from './useFocusActions'

const { t } = useI18n()
const { toast } = useToast()
const store = useFocusStore()
const { actualPrompt, customSnooze, snooze } = useFocusActions()

// ── Actual time ──
const actual = ref('')
watch(actualPrompt, (p) => (actual.value = p?.suggestion ?? ''))
const actualMin = computed(() => parseEstimate(actual.value))

async function saveActual() {
  const p = actualPrompt.value
  if (!p || actualMin.value == null) return
  actualPrompt.value = null
  try {
    await store.setActual(p.todo.id, actualMin.value)
  } catch (e) {
    toast.error(String(e))
  }
}

// ── Custom snooze ──
const until = ref('')
watch(customSnooze, (td) => {
  // Default: tomorrow at the start of the working day.
  if (!td) return
  const d = new Date()
  const [h, m] = (store.settings.work_start || '09:00').split(':').map(Number)
  until.value = toLocalInput(new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1, h, m).toISOString())
})
const untilIso = computed(() => fromLocalInput(until.value))

async function applySnooze() {
  const td = customSnooze.value
  if (!td || !untilIso.value) return
  customSnooze.value = null
  await snooze(td, untilIso.value)
}

function closeActual() {
  actualPrompt.value = null
}
function closeSnooze() {
  customSnooze.value = null
}

function onActualKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.isComposing) void saveActual()
}
</script>

<template>
  <Modal :open="!!actualPrompt" :title="t('focus.actual.title')" size="sm" @close="closeActual">
    <div class="space-y-2 p-4">
      <p class="text-xs text-muted-foreground">
        {{ t('focus.actual.hint', { estimate: formatEstimate(actualPrompt?.todo.estimate_min) }) }}
      </p>
      <Input
        v-model="actual"
        size="sm"
        class="w-40"
        :placeholder="t('focus.actual.placeholder')"
        :class="actualMin === undefined ? 'border-destructive' : ''"
        @keydown="onActualKey"
      />
      <p v-if="actualMin === undefined" class="text-[11px] text-destructive">{{ t('focus.drawer.estimateInvalid') }}</p>
    </div>
    <template #footer>
      <Button variant="ghost" @click="closeActual">{{ t('focus.actual.skip') }}</Button>
      <Button :disabled="actualMin == null" @click="saveActual">{{ t('focus.actual.save') }}</Button>
    </template>
  </Modal>

  <Modal :open="!!customSnooze" :title="t('focus.snooze.customTitle')" size="sm" @close="closeSnooze">
    <div class="p-4">
      <Input v-model="until" type="datetime-local" size="sm" class="w-full" />
    </div>
    <template #footer>
      <Button variant="ghost" @click="closeSnooze">{{ t('common.cancel') }}</Button>
      <Button :disabled="!untilIso" @click="applySnooze">{{ t('focus.snooze.apply') }}</Button>
    </template>
  </Modal>
</template>
