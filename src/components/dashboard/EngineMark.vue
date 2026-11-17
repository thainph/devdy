<script setup lang="ts">
// Engine badge: "C" (Claude) / "Cx" (Codex) in a tinted square.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { Engine } from '@/lib/runtimeDashboard'

const props = withDefaults(defineProps<{ engine: Engine; size?: 'sm' | 'md' | 'lg' }>(), { size: 'md' })
const { t } = useI18n()

const box = computed(() => ({
  sm: 'h-5 w-5 rounded text-[9px]',
  md: 'h-6 w-6 rounded-md text-[10px]',
  lg: 'h-8 w-8 rounded-md text-xs',
}[props.size]))
</script>

<template>
  <span
    class="grid shrink-0 place-items-center font-bold"
    :class="[box, engine === 'codex' ? 'bg-info/15 text-info' : 'bg-primary/15 text-primary']"
    :title="t(`dashboard.engine.${engine}`)"
  >
    <span aria-hidden="true">{{ engine === 'codex' ? 'Cx' : 'C' }}</span>
    <span class="sr-only">{{ t(`dashboard.engine.${engine}`) }}</span>
  </span>
</template>
