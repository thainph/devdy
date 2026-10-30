<script setup lang="ts">
// ⏸ Snooze menu of a todo (spec §3.2): 1 hour · this afternoon 14:00 (before 2 pm
// only) · tomorrow / next Monday at the start of the working day · pick a time.
// A snoozed todo also gets "Wake up now".
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlarmClockOff, CalendarClock, Pause } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
import { useFocusStore } from '@/stores/focus'
import { snoozePresets, type FocusTodo } from '@/lib/focus'
import { useFocusActions } from './useFocusActions'

const props = withDefaults(defineProps<{ todo: FocusTodo; size?: 'icon-sm' | 'icon' }>(), { size: 'icon-sm' })

const { t } = useI18n()
const store = useFocusStore()
const { snooze, customSnooze } = useFocusActions()

const workStart = computed(() => store.settings.work_start || '09:00')
// A function, not a computed: the menu body renders on each open, so "+1h" is from now.
const presets = () => snoozePresets(Date.now(), workStart.value)
const snoozed = computed(() => {
  const until = props.todo.snoozed_until ? Date.parse(props.todo.snoozed_until) : NaN
  return Number.isFinite(until) && until > Date.now()
})

function pickCustom() {
  customSnooze.value = props.todo
}

function label(preset: string): string {
  return t(`focus.snooze.${preset}`, { time: workStart.value })
}
</script>

<template>
  <DropdownMenu align="right">
    <template #trigger>
      <Button variant="ghost" :size="size" :title="t('focus.actions.snooze')" :aria-label="t('focus.actions.snooze')">
        <Pause class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </template>
    <DropdownItem v-for="p in presets()" :key="p.preset" @click="snooze(todo, p.at)">
      {{ label(p.preset) }}
    </DropdownItem>
    <DropdownSeparator />
    <DropdownItem @click="pickCustom">
      <CalendarClock class="h-3.5 w-3.5" :stroke-width="1.75" />
      {{ t('focus.snooze.custom') }}
    </DropdownItem>
    <DropdownItem v-if="snoozed" variant="primary" @click="snooze(todo, null)">
      <AlarmClockOff class="h-3.5 w-3.5" :stroke-width="1.75" />
      {{ t('focus.snooze.wake') }}
    </DropdownItem>
  </DropdownMenu>
</template>
