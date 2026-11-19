<script setup lang="ts">
// Running / Leftover / From last launch, as a tinted pill: success for live
// work, warning for anything nobody owns any more.
import { useI18n } from 'vue-i18n'
import type { ProcState } from '@/lib/backgroundProcesses'

defineProps<{ state: ProcState }>()
const { t } = useI18n()
</script>

<template>
  <span
    class="inline-flex items-center whitespace-nowrap rounded-full px-1.5 py-px text-[10px] font-semibold"
    :class="state === 'active' ? 'bg-success/12 text-success' : 'bg-warning/12 text-warning'"
  >
    {{ t(`processes.state.${state}`) }}
  </span>
</template>
