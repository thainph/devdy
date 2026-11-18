<script setup lang="ts">
// The show / hide-workers toggle inside a parent session's row or card, right
// after its title: "▸ 6 workers". It sits above the row's open-session button,
// so a click toggles without opening the session. Warning colour while
// collapsed with a worker that needs you.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronRight } from 'lucide-vue-next'

const props = defineProps<{ count: number; open: boolean; attention: number }>()
const emit = defineEmits<{ toggle: [] }>()
const { t } = useI18n()

const label = computed(() => {
  const head = props.open ? t('dashboard.tree.hide', { n: props.count }, props.count) : t('dashboard.tree.show', { n: props.count }, props.count)
  return !props.open && props.attention ? `${head} · ${t('dashboard.tree.needsYou', { n: props.attention })}` : head
})
</script>

<template>
  <button
    type="button"
    class="relative z-10 inline-flex h-5 shrink-0 cursor-pointer items-center gap-0.5 rounded-full border pr-2 pl-1 text-[10px] font-semibold whitespace-nowrap tabular-nums transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-primary motion-reduce:transition-none"
    :class="!open && attention ? 'border-warning/50 bg-warning/10 text-warning hover:bg-warning/20' : 'border-border bg-muted/60 text-muted-foreground hover:bg-accent hover:text-foreground'"
    :aria-expanded="open"
    :aria-label="label"
    :title="label"
    @click.stop="emit('toggle')"
  >
    <ChevronRight class="h-3 w-3 transition-transform motion-reduce:transition-none" :class="open && 'rotate-90'" :stroke-width="2.25" aria-hidden="true" />
    {{ t('dashboard.tree.pill', { n: count }, count) }}
  </button>
</template>
