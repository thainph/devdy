<script setup lang="ts">
/**
 * Setting → Working hours & reminders on the phone (mirrors the desktop's
 * Settings section): working hours and days, the morning summary time and
 * reminders on / off and per kind. The other settings (AI engine, Google
 * Calendar, calibration) are no longer shown: they are kept as loaded and saved
 * back untouched. The Host validates and answers with the stored settings.
 */
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, X } from 'lucide-vue-next'
import { Banner, Button, Input, ToggleSwitch } from '@/components/ui'
import { NOTIFY_KINDS, type FocusSettings } from '@/lib/focus'
import { focusSettingsError, normaliseFocusSettings, toggleWorkDay } from '@/lib/remoteFocus'
import type { WriteOutcome } from '../dataStore'

const props = defineProps<{
  /** The Host's settings; null while loading. */
  settings: FocusSettings | null
  failed: boolean
  busy: boolean
  /** The last refused write, translated. */
  error: string | null
  save: (settings: FocusSettings) => Promise<WriteOutcome>
}>()

const emit = defineEmits<{
  load: []
  'clear-error': []
}>()

const { t } = useI18n()

onMounted(() => emit('load'))

const form = ref<FocusSettings | null>(null)
/** A save is in flight: the Host's answer (normalised) replaces the form. */
const saving = ref(false)
const saved = ref(false)

const dirty = computed(
  () =>
    !!form.value &&
    !!props.settings &&
    JSON.stringify(normaliseFocusSettings(props.settings)) !== JSON.stringify(normaliseFocusSettings(form.value)),
)
watch(dirty, (d) => {
  if (d) saved.value = false
})

// The Host's copy arrives (or changes on the desktop): take it unless the
// user is mid-edit.
watch(
  () => props.settings,
  (s) => {
    if (s && (!form.value || !dirty.value || saving.value)) form.value = normaliseFocusSettings(s)
  },
  { immediate: true },
)

const invalid = computed(() => {
  const f = form.value
  if (!f) return null
  switch (focusSettingsError(f)) {
    case 'hours':
      return t('focus.settings.invalidHours')
    case 'days':
      return t('focus.settings.noDays')
    case 'morning':
      return t('focus.settings.reminders.invalidMorning')
    default:
      return null
  }
})

const DAYS = [1, 2, 3, 4, 5, 6, 7]
function flipDay(d: number): void {
  if (form.value) form.value.work_days = toggleWorkDay(form.value.work_days, d)
}
function flipKind(k: string): void {
  if (form.value) form.value.notify_kinds = { ...form.value.notify_kinds, [k]: !form.value.notify_kinds[k] }
}

async function submit(): Promise<void> {
  const f = form.value
  if (!f || invalid.value || props.busy || !dirty.value) return
  saving.value = true
  try {
    const out = await props.save(normaliseFocusSettings(f))
    if (out.ok) saved.value = true
  } finally {
    saving.value = false
  }
}

const heading = 'mt-4 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
const card = 'ui-card overflow-hidden rounded-xl border border-border bg-card'
const row = 'flex items-center gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 text-sm'
</script>

<template>
  <div class="flex h-full min-h-0 flex-col text-foreground">
    <Banner
      v-if="error"
      tone="destructive"
      class="shrink-0 gap-2 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{ error }}
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
      <div v-if="!form && failed" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <Button variant="outline" @click="emit('load')">{{ t('controller.data.retry') }}</Button>
      </div>
      <p v-else-if="!form" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>

      <form v-else @submit.prevent="submit">
        <p class="px-0.5 text-[11px] leading-snug text-muted-foreground">{{ t('focus.settings.hint') }}</p>

        <h2 :class="heading">{{ t('focus.settings.workHours') }}</h2>
        <div :class="card">
          <label :class="row">
            <span class="min-w-0 flex-1">{{ t('focus.settings.workStart') }}</span>
            <Input v-model="form.work_start" type="time" size="md" class="w-32" />
          </label>
          <label :class="row">
            <span class="min-w-0 flex-1">{{ t('focus.settings.workEnd') }}</span>
            <Input v-model="form.work_end" type="time" size="md" class="w-32" />
          </label>
          <div class="px-3 py-2.5">
            <span class="mb-2 block text-sm">{{ t('focus.settings.workDays') }}</span>
            <div class="grid grid-cols-7 gap-1">
              <button
                v-for="d in DAYS"
                :key="d"
                type="button"
                class="rounded-md border py-1.5 text-xs transition-colors cursor-pointer"
                :class="
                  form.work_days.includes(d)
                    ? 'border-primary bg-primary/15 font-semibold text-foreground'
                    : 'border-border bg-secondary text-muted-foreground'
                "
                :aria-pressed="form.work_days.includes(d)"
                @click="flipDay(d)"
              >
                {{ t(`focus.settings.days.${d}`) }}
              </button>
            </div>
          </div>
        </div>

        <h2 :class="heading">{{ t('focus.settings.reminders.title') }}</h2>
        <div :class="card">
          <div :class="row">
            <span class="min-w-0 flex-1 font-medium">{{ t('controller.focus.settings.notify') }}</span>
            <ToggleSwitch
              size="md"
              :checked="form.notify_enabled"
              :label="t('controller.focus.settings.notify')"
              @toggle="form.notify_enabled = !form.notify_enabled"
            />
          </div>
          <label :class="row">
            <span class="min-w-0 flex-1">{{ t('focus.settings.reminders.morningTime') }}</span>
            <Input v-model="form.morning_time" type="time" size="md" class="w-32" />
          </label>
          <div v-for="k in NOTIFY_KINDS" :key="k" :class="row">
            <div class="min-w-0 flex-1" :class="form.notify_enabled ? '' : 'opacity-50'">
              <p>{{ t(`focus.settings.reminders.kinds.${k}`) }}</p>
              <p class="text-[11px] leading-snug text-muted-foreground">{{ t(`focus.settings.reminders.kindHints.${k}`) }}</p>
            </div>
            <ToggleSwitch
              size="md"
              :checked="form.notify_kinds[k] !== false"
              :disabled="!form.notify_enabled"
              :label="t(`focus.settings.reminders.kinds.${k}`)"
              @toggle="flipKind(k)"
            />
          </div>
        </div>
        <p class="mt-1.5 px-0.5 text-[11px] leading-snug text-muted-foreground">
          {{ t('controller.focus.settings.notifyHint') }}
        </p>

        <p v-if="invalid" class="mt-3 px-0.5 text-xs text-destructive">{{ invalid }}</p>
        <div class="mt-3 pb-[env(safe-area-inset-bottom)]">
          <Button type="submit" size="md" class="w-full" :disabled="busy || !!invalid || !dirty">
            {{ busy ? t('controller.data.saving') : saved && !dirty ? t('controller.focus.settings.saved') : t('controller.data.save') }}
          </Button>
        </div>
      </form>
    </main>
  </div>
</template>
