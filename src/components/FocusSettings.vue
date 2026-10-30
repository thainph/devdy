<script setup lang="ts">
// Settings → "Focus": the working hours the board counts time in (spec §2, §5.2) —
// start / end of the working day and the working days. Deadlines without a time
// fall due at the end of the working day (BR-03). The other `focus.*` settings
// (reminders, Calendar, AI) are kept as loaded and saved back untouched.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Target } from 'lucide-vue-next'
import { Button, Card, Input } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { DEFAULT_FOCUS_SETTINGS, type FocusSettings } from '@/lib/focus'

const { t } = useI18n()
const { toast } = useToast()
const store = useFocusStore()

const form = ref<FocusSettings>({ ...DEFAULT_FOCUS_SETTINGS })
const loading = ref(true)
const saving = ref(false)

const DAYS = [1, 2, 3, 4, 5, 6, 7]

onMounted(async () => {
  await store.loadSettings()
  form.value = { ...store.settings, work_days: [...store.settings.work_days] }
  loading.value = false
})

function toggleDay(d: number) {
  const days = new Set(form.value.work_days)
  if (days.has(d)) days.delete(d)
  else days.add(d)
  form.value.work_days = [...days].sort((a, b) => a - b)
}

const error = computed(() => {
  const { work_start: s, work_end: e } = form.value
  if (!/^\d{2}:\d{2}$/.test(s) || !/^\d{2}:\d{2}$/.test(e) || e <= s) return t('focus.settings.invalidHours')
  if (form.value.work_days.length === 0) return t('focus.settings.noDays')
  return null
})

const dirty = computed(() => {
  const s = store.settings
  return (
    form.value.work_start !== s.work_start ||
    form.value.work_end !== s.work_end ||
    form.value.work_days.join(',') !== s.work_days.join(',')
  )
})

async function save() {
  if (error.value || saving.value) return
  saving.value = true
  try {
    const saved = await store.saveSettings({ ...store.settings, ...form.value })
    form.value = { ...saved, work_days: [...saved.work_days] }
    toast.success(t('focus.settings.saved'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <div class="flex items-center gap-2">
      <Target class="h-4 w-4 text-muted-foreground" :stroke-width="1.75" />
      <span class="text-xs font-semibold">{{ t('focus.settings.title') }}</span>
    </div>
    <p class="text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.hint') }}</p>

    <div class="space-y-1.5">
      <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.settings.workHours') }}</label>
      <div class="flex items-center gap-2">
        <Input v-model="form.work_start" type="time" size="sm" class="w-28" :disabled="loading" :aria-label="t('focus.settings.workStart')" />
        <span class="text-muted-foreground">–</span>
        <Input v-model="form.work_end" type="time" size="sm" class="w-28" :disabled="loading" :aria-label="t('focus.settings.workEnd')" />
      </div>
    </div>

    <div class="space-y-1.5">
      <label class="block text-[11px] font-medium text-muted-foreground">{{ t('focus.settings.workDays') }}</label>
      <div class="flex flex-wrap gap-1">
        <button
          v-for="d in DAYS"
          :key="d"
          type="button"
          class="h-7 w-10 cursor-pointer rounded-md border text-xs transition-colors"
          :class="form.work_days.includes(d)
            ? 'border-primary bg-primary text-primary-foreground'
            : 'border-border bg-background text-muted-foreground hover:bg-accent hover:text-foreground'"
          :aria-pressed="form.work_days.includes(d)"
          :disabled="loading"
          @click="toggleDay(d)"
        >
          {{ t(`focus.settings.days.${d}`) }}
        </button>
      </div>
    </div>

    <p v-if="error" class="text-[11px] text-destructive">{{ error }}</p>

    <div class="flex justify-end">
      <Button size="sm" :disabled="loading || !dirty || !!error || saving" @click="save">{{ t('focus.settings.save') }}</Button>
    </div>
  </Card>
</template>
