<script setup lang="ts">
// Frame of one Work Dashboard widget: a Card whose header carries the icon,
// title, count pill, scope note, the data's age ("updated 5m ago", flagged when
// stale) and a refresh spinner; the body lists at most WIDGET_ROWS rows, then a
// "See all" footer opening the screen / window that owns the data. Empty,
// error and "not set up" states go through the slots so every widget reads
// the same way.
import { computed, type Component } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowRight, Loader2 } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { ageMs, STALE_MS } from '@/lib/workDashboard'
import { useDurationLabel } from './useDurationLabel'

const props = withDefaults(defineProps<{
  title: string
  icon: Component
  /** Count pill (what needs attention); hidden when null. */
  count?: number | null
  countTitle?: string
  /** Short scope note after the title (e.g. "GitHub PRs"). */
  scope?: string
  updatedAt?: number | null
  now: number
  loading?: boolean
  /** "See all" footer label; hidden when empty. */
  seeAll?: string
  /** Data older than this is flagged as stale (ms). */
  staleAfter?: number
}>(), {
  count: null,
  countTitle: undefined,
  scope: undefined,
  updatedAt: null,
  loading: false,
  seeAll: '',
  staleAfter: STALE_MS,
})

const emit = defineEmits<{ seeAll: [] }>()

defineSlots<{
  default?: () => unknown
  /** Extra header controls, right-aligned. */
  actions?: () => unknown
}>()

const { t } = useI18n()
const { ago } = useDurationLabel()

const age = computed(() => ageMs(props.updatedAt, props.now))
const stale = computed(() => age.value !== null && age.value > props.staleAfter)
</script>

<template>
  <Card class="flex min-w-0 flex-col" body-class="flex min-h-0 flex-1 flex-col">
    <template #header>
      <component :is="icon" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
      <span class="truncate text-xs font-semibold">{{ title }}</span>
      <span
        v-if="count !== null"
        class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full px-1.5 text-[10px] font-medium"
        :class="count > 0 ? 'bg-warning text-warning-foreground' : 'bg-muted text-muted-foreground'"
        :title="countTitle"
      >{{ count }}</span>
      <span v-if="scope" class="truncate text-[11px] text-muted-foreground">· {{ scope }}</span>
      <span class="ml-auto flex shrink-0 items-center gap-1.5 text-[11px] text-muted-foreground">
        <Loader2 v-if="loading" class="h-3 w-3 animate-spin" :stroke-width="2" />
        <span
          v-if="age !== null"
          :class="stale && 'text-warning'"
          :title="updatedAt ? new Date(updatedAt).toLocaleString() : undefined"
        >{{ stale ? t('dashboard.staleData', { age: ago(age) }) : t('dashboard.updated', { age: ago(age) }) }}</span>
        <slot name="actions" />
      </span>
    </template>

    <div class="flex-1">
      <slot />
    </div>

    <div v-if="seeAll" class="flex justify-end border-t border-border/50 px-2 py-1">
      <Button variant="ghost" size="xs" @click="emit('seeAll')">
        {{ seeAll }}
        <ArrowRight class="h-3 w-3" :stroke-width="2" />
      </Button>
    </div>
  </Card>
</template>
