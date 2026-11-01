<script setup lang="ts">
// Inline status message (error / warning / success / info): tinted box with a
// tone icon, the message, and optional trailing actions (retry, dismiss…).
// Replaces the hand-copied `border-<tone>/30 bg-<tone>/10 …` blocks.
import { computed, type Component } from 'vue'
import { AlertCircle, AlertTriangle, CheckCircle2, Info, Loader2 } from 'lucide-vue-next'
import { cn } from '@/lib/utils'

const props = withDefaults(defineProps<{
  tone?: 'destructive' | 'warning' | 'success' | 'info'
  /** Override the tone's default icon; `null` hides it. */
  icon?: Component | null
  /** Spinner instead of the icon while something is in progress. */
  loading?: boolean
  class?: string
}>(), {
  tone: 'warning',
  icon: undefined,
  loading: false,
  class: undefined,
})

defineSlots<{
  default?: () => unknown
  actions?: () => unknown
}>()

// Full class strings so Tailwind can see them.
const TONE = {
  destructive: { box: 'border-destructive/30 bg-destructive/10', icon: 'text-destructive', glyph: AlertCircle },
  warning: { box: 'border-warning/30 bg-warning/10', icon: 'text-warning', glyph: AlertTriangle },
  success: { box: 'border-success/30 bg-success/10', icon: 'text-success', glyph: CheckCircle2 },
  info: { box: 'border-info/30 bg-info/10', icon: 'text-info', glyph: Info },
} as const

const glyph = computed(() =>
  props.loading ? Loader2 : props.icon === undefined ? TONE[props.tone].glyph : props.icon,
)
</script>

<template>
  <div
    :class="cn('flex items-start gap-1.5 rounded-md border px-2 py-1.5 text-[11px] text-foreground', TONE[tone].box, props.class)"
    :role="tone === 'destructive' ? 'alert' : 'status'"
  >
    <component :is="glyph" v-if="glyph" class="mt-px h-3.5 w-3.5 shrink-0" :class="[TONE[tone].icon, loading && 'animate-spin']" :stroke-width="2" />
    <div class="min-w-0 flex-1 break-words">
      <slot />
    </div>
    <div v-if="$slots.actions" class="flex shrink-0 items-center gap-1">
      <slot name="actions" />
    </div>
  </div>
</template>
