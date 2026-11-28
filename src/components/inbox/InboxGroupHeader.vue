<script setup lang="ts">
// Collapsible group header of the inbox lists (PR Inbox, GitHub Mentions):
// chevron · icon · name · count pill · optional extras, then actions on the
// right. The name truncates first; counts stay.
import { ChevronRight } from 'lucide-vue-next'

withDefaults(defineProps<{
  label: string
  count: number
  collapsed: boolean
  /** Repo names read better in mono. */
  mono?: boolean
}>(), { mono: false })

const emit = defineEmits<{ toggle: [] }>()
</script>

<template>
  <div class="group/header flex min-w-0 items-center gap-2">
    <button
      type="button"
      class="flex min-w-0 flex-1 items-center gap-2 text-left cursor-pointer"
      :aria-expanded="!collapsed"
      @click="emit('toggle')"
    >
      <ChevronRight
        class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
        :class="{ 'rotate-90': !collapsed }"
        :stroke-width="2"
      />
      <slot name="icon" />
      <span
        class="min-w-0 truncate text-xs font-semibold transition-colors group-hover/header:text-primary"
        :class="{ 'font-mono': mono }"
      >
        {{ label }}
      </span>
      <span class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground">
        {{ count }}
      </span>
      <slot name="extra" />
    </button>
    <div v-if="$slots.actions" class="flex shrink-0 items-center gap-1">
      <slot name="actions" />
    </div>
  </div>
</template>
