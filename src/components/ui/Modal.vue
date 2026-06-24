<script setup lang="ts">
// Shared dialog: dimmed overlay + centered panel, Esc / overlay-click to close,
// optional title + close button + footer. Replaces the hand-rolled
// `fixed inset-0 z-50 …` overlays repeated across the app.
import { computed, watch, onBeforeUnmount, useSlots } from 'vue'
import { X } from 'lucide-vue-next'
import { cn } from '@/lib/utils'

const props = withDefaults(defineProps<{
  open: boolean
  title?: string
  size?: 'sm' | 'md' | 'lg' | 'xl' | 'full'
  /** When false, hides the X and disables Esc / overlay-click dismissal. */
  closable?: boolean
  /** Caps panel height and makes the body scroll. */
  scrollBody?: boolean
  /** Suppress the built-in header (the body provides its own). Esc / overlay
   *  dismissal still work when `closable`. */
  hideHeader?: boolean
}>(), {
  size: 'md',
  closable: true,
  scrollBody: false,
  hideHeader: false,
})

const emit = defineEmits<{ close: []; 'update:open': [value: boolean] }>()
const slots = useSlots()

const SIZES = { sm: 'max-w-md', md: 'max-w-lg', lg: 'max-w-2xl', xl: 'max-w-3xl', full: 'max-w-[96vw]' }
const panelClass = computed(() =>
  cn(
    'w-full rounded-lg border border-border bg-card shadow-xl',
    SIZES[props.size],
    props.scrollBody &&
      (props.size === 'full' ? 'h-[94vh] flex flex-col' : 'max-h-[80vh] flex flex-col'),
  ),
)

function close() {
  if (!props.closable) return
  emit('close')
  emit('update:open', false)
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') close()
}

watch(
  () => props.open,
  (open) => {
    if (open) window.addEventListener('keydown', onKey)
    else window.removeEventListener('keydown', onKey)
  },
)
onBeforeUnmount(() => window.removeEventListener('keydown', onKey))
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
    @click.self="close"
  >
    <div :class="panelClass">
      <!-- Header -->
      <div
        v-if="!hideHeader && (title || slots.header || closable)"
        class="flex items-center gap-2 border-b border-border px-4 py-3 shrink-0"
      >
        <slot name="header">
          <h3 class="text-sm font-semibold flex-1">{{ title }}</h3>
        </slot>
        <button
          v-if="closable"
          class="flex items-center justify-center h-6 w-6 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
          aria-label="Close"
          title="Close (Esc)"
          @click="close"
        >
          <X class="h-4 w-4" :stroke-width="1.75" />
        </button>
      </div>

      <!-- Body -->
      <div :class="scrollBody ? 'flex-1 overflow-auto min-h-0' : ''">
        <slot />
      </div>

      <!-- Footer -->
      <div v-if="slots.footer" class="flex items-center justify-end gap-2 border-t border-border px-4 py-3 shrink-0">
        <slot name="footer" />
      </div>
    </div>
  </div>
</template>
