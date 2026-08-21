<script setup lang="ts">
// Side drawer: dimmed overlay + panel that slides in from the left or right.
// Esc / overlay-click to close (when closable). Mirrors Modal's API but anchors
// to an edge and animates with a slide instead of a centered fade.
import { computed, watch, onBeforeUnmount, useSlots } from 'vue'
import { X } from 'lucide-vue-next'
import { cn } from '@/lib/utils'

const props = withDefaults(defineProps<{
  open: boolean
  title?: string
  side?: 'left' | 'right'
  size?: 'sm' | 'md' | 'lg' | 'xl'
  /** When false, hides the X and disables Esc / overlay-click dismissal. */
  closable?: boolean
  /** When false, clicking the dimmed overlay won't close the drawer (X / Esc still work). */
  dismissOnOverlay?: boolean
}>(), {
  side: 'right',
  size: 'md',
  closable: true,
  dismissOnOverlay: true,
})

const emit = defineEmits<{ close: []; 'update:open': [value: boolean] }>()
const slots = useSlots()

const SIZES = { sm: 'max-w-sm', md: 'max-w-md', lg: 'max-w-xl', xl: 'max-w-3xl' }
const panelClass = computed(() =>
  cn(
    'drawer-panel relative flex h-full w-full flex-col bg-popover shadow-2xl shadow-black/40',
    SIZES[props.size],
    props.side === 'left' ? 'border-r border-border/70' : 'border-l border-border/70',
  ),
)

function close() {
  if (!props.closable) return
  emit('close')
  emit('update:open', false)
}

function onOverlayClick() {
  if (!props.dismissOnOverlay) return
  close()
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
  <Teleport to="body">
    <Transition name="drawer">
      <div
        v-if="open"
        class="fixed inset-0 z-40 flex bg-black/60"
        :class="[side === 'left' ? 'justify-start side-left' : 'justify-end side-right']"
        @click.self="onOverlayClick"
      >
        <div :class="panelClass">
          <!-- Header -->
          <div
            v-if="title || slots.header || closable"
            class="flex items-center gap-2 border-b border-border px-4 py-3 shrink-0"
          >
            <slot name="header">
              <h3 class="text-sm font-semibold flex-1 truncate">{{ title }}</h3>
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
          <div class="flex-1 overflow-auto min-h-0">
            <slot />
          </div>

          <!-- Footer -->
          <div
            v-if="slots.footer"
            class="flex items-center gap-2 border-t border-border px-4 py-3 shrink-0"
          >
            <slot name="footer" />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
/* Overlay fades; the panel slides from its anchored edge. */
.drawer-enter-active,
.drawer-leave-active {
  transition: opacity 0.2s ease;
}
.drawer-enter-from,
.drawer-leave-to {
  opacity: 0;
}
.drawer-enter-active .drawer-panel,
.drawer-leave-active .drawer-panel {
  transition: transform 0.25s cubic-bezier(0.4, 0, 0.2, 1);
}
.drawer-enter-from.side-left .drawer-panel,
.drawer-leave-to.side-left .drawer-panel {
  transform: translateX(-100%);
}
.drawer-enter-from.side-right .drawer-panel,
.drawer-leave-to.side-right .drawer-panel {
  transform: translateX(100%);
}
</style>
