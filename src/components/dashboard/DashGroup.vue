<script setup lang="ts">
// A labelled group of rows inside a dashboard card ("Waiting on you · 2"),
// with the rows that did not fit folded into one "+N more" line.
import { useI18n } from 'vue-i18n'

withDefaults(defineProps<{
  label: string
  count?: number | null
  /** Rows left out to keep the card short. */
  hidden?: number
}>(), { count: null, hidden: 0 })

const emit = defineEmits<{ more: [] }>()

const { t } = useI18n()
</script>

<template>
  <section class="pt-1">
    <h3 class="px-4 pt-1.5 pb-0.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
      {{ label }}<template v-if="count !== null"> · {{ count }}</template>
    </h3>
    <ul class="divide-y divide-border/40">
      <slot />
      <li v-if="hidden > 0">
        <button
          type="button"
          class="w-full cursor-pointer px-4 py-1.5 text-left text-[11px] text-muted-foreground hover:bg-accent/50 hover:text-foreground"
          @click="emit('more')"
        >
          {{ t('dashboard.more', { n: hidden }) }}
        </button>
      </li>
    </ul>
  </section>
</template>
