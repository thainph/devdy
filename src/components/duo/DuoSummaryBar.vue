<script setup lang="ts">
/**
 * Collapsed stand-in for the config form: one row of chips summarising the
 * running duo, so the conversation gets the full height. Clicking it reopens
 * the form (read-only while the duo is active).
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Settings2 } from 'lucide-vue-next'
import { useOrchestratorStore } from '@/stores/orchestrator'

defineEmits<{ expand: [] }>()

const { t } = useI18n()
const orch = useOrchestratorStore()

const goalShort = computed(() => {
  const g = orch.state.goal.trim().replace(/\s+/g, ' ')
  return g.length > 90 ? `${g.slice(0, 90)}…` : g
})
</script>

<template>
  <button
    class="flex w-full items-center gap-2 border-b border-border bg-card/40 px-4 py-1.5 text-left text-[11px] transition-colors hover:bg-accent/40 cursor-pointer shrink-0"
    :title="t('duo.action.expand')"
    @click="$emit('expand')"
  >
    <span class="font-mono text-muted-foreground shrink-0">
      {{ orch.state.designerEngine }} ↔ {{ orch.state.reviewerEngine }}
    </span>
    <span v-if="orch.state.permissionMode" class="rounded bg-muted px-1.5 py-px text-muted-foreground shrink-0">
      {{ orch.state.permissionMode }}
    </span>
    <span class="rounded bg-muted px-1.5 py-px text-muted-foreground shrink-0">
      {{ t('duo.roundCounter', { n: orch.state.round, max: orch.state.maxRounds }) }}
    </span>
    <span v-if="goalShort" class="truncate text-muted-foreground/80">{{ goalShort }}</span>
    <Settings2 class="ml-auto h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="2" />
  </button>
</template>
