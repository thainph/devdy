<script setup lang="ts">
// Settings → "Working hours & reminders" (docs/focus-board-spec.md §5.2):
// working hours (deadlines without a time fall due at the end of the working
// day, BR-03) and reminders (§3.4: master switch, per-kind switches, morning
// summary time). The other settings (Calendar, calibration, AI) are no longer
// shown since the board became the lateness view; they are kept as loaded and
// saved back untouched.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Bell, Target } from 'lucide-vue-next'
import { Button, Card, Input, ToggleSwitch } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { DEFAULT_FOCUS_SETTINGS, NOTIFY_KINDS, notifyKindsOf, type FocusSettings } from '@/lib/focus'

const { t } = useI18n()
const { toast } = useToast()
const store = useFocusStore()

const form = ref<FocusSettings>({ ...DEFAULT_FOCUS_SETTINGS })
const loading = ref(true)
const saving = ref(false)

const DAYS = [1, 2, 3, 4, 5, 6, 7]

/** A detached copy with every reminder kind present. */
function normalised(s: FocusSettings): FocusSettings {
  return {
    ...s,
    work_days: [...s.work_days],
    calendar_ids: [...s.calendar_ids],
    notify_kinds: notifyKindsOf(s.notify_kinds),
  }
}

onMounted(async () => {
  await store.loadSettings()
  form.value = normalised(store.settings)
  loading.value = false
})

function toggleDay(d: number) {
  const days = new Set(form.value.work_days)
  if (days.has(d)) days.delete(d)
  else days.add(d)
  form.value.work_days = [...days].sort((a, b) => a - b)
}

const HM = /^\d{2}:\d{2}$/
const error = computed(() => {
  const { work_start: s, work_end: e, morning_time: m } = form.value
  if (!HM.test(s) || !HM.test(e) || e <= s) return t('focus.settings.invalidHours')
  if (form.value.work_days.length === 0) return t('focus.settings.noDays')
  if (!HM.test(m)) return t('focus.settings.reminders.invalidMorning')
  return null
})

const dirty = computed(() => JSON.stringify(normalised(store.settings)) !== JSON.stringify(normalised(form.value)))

async function save() {
  if (error.value || saving.value) return
  saving.value = true
  try {
    const saved = await store.saveSettings(normalised(form.value))
    form.value = normalised(saved)
    toast.success(t('focus.settings.saved'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Card body-class="p-4 space-y-5">
    <div class="flex items-center gap-2">
      <Target class="h-4 w-4 text-muted-foreground" :stroke-width="1.75" />
      <span class="text-xs font-semibold">{{ t('focus.settings.title') }}</span>
    </div>
    <p class="text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.hint') }}</p>

    <!-- Working hours -->
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

    <!-- Reminders -->
    <div class="space-y-2 border-t border-border/60 pt-4">
      <div class="flex items-center justify-between gap-3">
        <div class="min-w-0">
          <p class="flex items-center gap-1.5 text-xs font-medium">
            <Bell class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            {{ t('focus.settings.reminders.title') }}
          </p>
          <p class="mt-0.5 text-[11px] leading-relaxed text-muted-foreground">{{ t('focus.settings.reminders.hint') }}</p>
        </div>
        <ToggleSwitch
          size="md"
          :checked="form.notify_enabled"
          :label="t('focus.settings.reminders.title')"
          :disabled="loading"
          @toggle="form.notify_enabled = !form.notify_enabled"
        />
      </div>
      <div v-if="form.notify_enabled" class="space-y-2 pl-5">
        <div class="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
          <label v-for="k in NOTIFY_KINDS" :key="k" class="flex cursor-pointer items-start gap-2 text-xs">
            <input v-model="form.notify_kinds[k]" type="checkbox" class="mt-0.5 accent-primary" :disabled="loading" />
            <span class="min-w-0">
              <span class="block">{{ t(`focus.settings.reminders.kinds.${k}`) }}</span>
              <span class="block text-[10px] leading-snug text-muted-foreground">{{ t(`focus.settings.reminders.kindHints.${k}`) }}</span>
            </span>
          </label>
        </div>
        <div class="flex items-center gap-2">
          <label class="text-[11px] text-muted-foreground">{{ t('focus.settings.reminders.morningTime') }}</label>
          <Input
            v-model="form.morning_time"
            type="time"
            size="sm"
            class="w-28"
            :disabled="loading || !form.notify_kinds.morning"
            :aria-label="t('focus.settings.reminders.morningTime')"
          />
        </div>
        <p class="text-[10px] leading-relaxed text-muted-foreground">{{ t('focus.settings.reminders.workHoursOnly') }}</p>
      </div>
    </div>

    <p v-if="error" class="text-[11px] text-destructive">{{ error }}</p>

    <div class="flex justify-end">
      <Button size="sm" :disabled="loading || !dirty || !!error || saving" @click="save">{{ t('focus.settings.save') }}</Button>
    </div>
  </Card>
</template>
