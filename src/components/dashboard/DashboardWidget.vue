<script setup lang="ts">
// Frame of one Work Dashboard card: a Card whose header carries a drag handle
// (the cards are user-ordered), the icon and title, the data's age ("updated
// 5m ago", flagged when stale), a refresh spinner, the link to the screen that
// owns the data and "move to the top" (the keyboard path of drag & drop).
// The body opens with the card's answer — one big number and words (slot
// `answer`) — then its grouped rows. Empty, error and "not set up" states go
// through the default slot so every card reads the same way.
import { computed, type Component } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowUpToLine, ChevronRight, GripVertical, Loader2 } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { ageMs, STALE_MS } from '@/lib/workDashboard'
import { useDurationLabel } from './useDurationLabel'

const props = withDefaults(defineProps<{
  title: string
  icon: Component
  /** Short scope note after the title (e.g. "GitHub PRs"). */
  scope?: string
  updatedAt?: number | null
  now: number
  loading?: boolean
  /** Label of the link to the owning screen; hidden when empty. */
  seeAll?: string
  /** Data older than this is flagged as stale (ms). */
  staleAfter?: number
  /** Show the drag handle + "move to the top" (false for a fixed panel). */
  movable?: boolean
  /** Already the first card: no "move to the top". */
  first?: boolean
}>(), {
  scope: undefined,
  updatedAt: null,
  loading: false,
  seeAll: '',
  staleAfter: STALE_MS,
  movable: false,
  first: false,
})

const emit = defineEmits<{ seeAll: []; moveToFront: []; dragStart: [e: DragEvent]; dragEnd: [] }>()

defineSlots<{
  /** The card's answer: big number + words. */
  answer?: () => unknown
  default?: () => unknown
}>()

const { t } = useI18n()
const { ago } = useDurationLabel()

const age = computed(() => ageMs(props.updatedAt, props.now))
const stale = computed(() => age.value !== null && age.value > props.staleAfter)
</script>

<template>
  <Card class="@container/card flex min-w-0 flex-col" body-class="flex min-h-0 flex-1 flex-col">
    <template #header>
      <span
        v-if="movable"
        class="-ml-1.5 flex h-5 w-4 shrink-0 cursor-grab items-center justify-center text-muted-foreground/60 hover:text-foreground active:cursor-grabbing"
        draggable="true"
        :title="t('dashboard.order.drag')"
        aria-hidden="true"
        @dragstart="emit('dragStart', $event)"
        @dragend="emit('dragEnd')"
      >
        <GripVertical class="h-3.5 w-3.5" :stroke-width="1.75" />
      </span>
      <component :is="icon" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
      <h2 class="truncate text-xs font-semibold">{{ title }}</h2>
      <span v-if="scope" class="hidden truncate text-[11px] text-muted-foreground @xl/card:inline">· {{ scope }}</span>
      <span class="ml-auto flex shrink-0 items-center gap-1 text-[11px] text-muted-foreground">
        <Loader2 v-if="loading" class="h-3 w-3 animate-spin" :stroke-width="2" />
        <span
          v-if="age !== null"
          class="hidden @lg/card:inline"
          :class="stale && 'text-warning'"
          :title="updatedAt ? new Date(updatedAt).toLocaleString() : undefined"
        >{{ stale ? t('dashboard.staleData', { age: ago(age) }) : t('dashboard.updated', { age: ago(age) }) }}</span>
        <Button v-if="seeAll" variant="ghost" size="xs" class="-my-1" @click="emit('seeAll')">
          {{ seeAll }}
          <ChevronRight class="h-3 w-3" :stroke-width="2" />
        </Button>
        <Button
          v-if="movable && !first"
          variant="ghost"
          size="icon-xs"
          :title="t('dashboard.order.moveToFront', { name: title })"
          :aria-label="t('dashboard.order.moveToFront', { name: title })"
          @click="emit('moveToFront')"
        >
          <ArrowUpToLine class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </span>
    </template>

    <div v-if="$slots.answer" class="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 px-4 pt-3 pb-2">
      <slot name="answer" />
    </div>
    <div class="flex-1 pb-1">
      <slot />
    </div>
  </Card>
</template>
