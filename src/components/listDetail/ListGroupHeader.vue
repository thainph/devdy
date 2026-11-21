<script setup lang="ts">
// A light group header of a list-detail sidebar: a small uppercase label that
// sticks to the top of the scrolling list, with the group's count and an
// optional red "N late" pill.
//
//   ▾ ICONTRUCTION  [2 late]                          5  [#actions]
//
// - `collapsed` + `toggle`: the chevron; `collapsible: false` drops it.
// - `late` / `lateTitle`: the red pill and its tooltip (hidden when 0).
// - `#actions`: hover-revealed controls on the right (e.g. delete the folder);
//   use Button size="icon-xs".
// Place it as a direct child of the scroll container so `sticky` works.
import { ChevronDown, ChevronRight } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

withDefaults(
  defineProps<{
    label: string
    count?: number | null
    late?: number
    lateTitle?: string
    collapsed?: boolean
    collapsible?: boolean
  }>(),
  { count: null, late: 0, lateTitle: undefined, collapsed: false, collapsible: true },
)

const emit = defineEmits<{ toggle: [] }>()

const { t } = useI18n()
</script>

<template>
  <div class="list-group-header group/folder sticky top-0 z-[1] flex items-center gap-1 bg-sidebar/90 pr-1 backdrop-blur-sm">
    <button
      type="button"
      class="flex min-w-0 flex-1 items-center gap-1 px-1.5 pb-1 pt-2 text-left text-[10px] font-semibold uppercase tracking-wider text-muted-foreground"
      :class="collapsible ? 'cursor-pointer hover:text-foreground' : 'cursor-default'"
      :aria-expanded="collapsible ? !collapsed : undefined"
      :disabled="!collapsible"
      @click="emit('toggle')"
    >
      <component
        :is="collapsed ? ChevronRight : ChevronDown"
        v-if="collapsible"
        class="h-3 w-3 shrink-0"
        :stroke-width="2"
      />
      <span class="min-w-0 truncate">{{ label }}</span>
      <span
        v-if="late"
        class="shrink-0 rounded-full bg-destructive/15 px-1.5 text-[10px] font-semibold normal-case tracking-normal tabular-nums text-destructive"
        :title="lateTitle"
      >
        {{ t('listPane.lateCount', { n: late }) }}
      </span>
      <span v-if="count != null" class="ml-auto shrink-0 pl-1 font-medium normal-case tracking-normal tabular-nums">
        {{ count }}
      </span>
    </button>
    <slot name="actions" />
  </div>
</template>
