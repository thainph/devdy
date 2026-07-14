<script setup lang="ts">
// App-wide toast stack. Mount once in App.vue; content is driven by useToast.
import { useToast, type ToastVariant } from '@/composables/useToast'
import { CheckCircle2, XCircle, Info } from 'lucide-vue-next'

const { state, dismiss } = useToast()

const ICONS = { success: CheckCircle2, error: XCircle, info: Info }
const TONES: Record<ToastVariant, string> = {
  success: 'text-emerald-500',
  error: 'text-destructive',
  info: 'text-primary',
}
</script>

<template>
  <Teleport to="body">
    <div class="fixed bottom-4 right-4 z-[100] flex flex-col gap-2 pointer-events-none">
      <TransitionGroup
        enter-active-class="transition duration-200 ease-out"
        enter-from-class="opacity-0 translate-y-2"
        enter-to-class="opacity-100 translate-y-0"
        leave-active-class="transition duration-150 ease-in absolute right-0"
        leave-from-class="opacity-100"
        leave-to-class="opacity-0 translate-y-1"
      >
        <div
          v-for="t in state.items"
          :key="t.id"
          class="pointer-events-auto flex items-center gap-2 min-w-56 max-w-sm bg-card border border-border rounded-lg shadow-md px-3.5 py-2.5 text-xs cursor-pointer"
          role="status"
          @click="dismiss(t.id)"
        >
          <component
            :is="ICONS[t.variant]"
            class="h-4 w-4 shrink-0"
            :class="TONES[t.variant]"
            :stroke-width="2"
          />
          <span class="flex-1 text-foreground leading-snug">{{ t.message }}</span>
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>
