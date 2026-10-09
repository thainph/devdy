<script setup lang="ts">
// The pin toggle of every pop-out window (item, capture): pinned keeps the window
// above other apps even after it loses focus (see composables/useFloatingWindow).
// The host owns the state, since useFloatingWindow must run once per window.
import { useI18n } from 'vue-i18n'
import { Pin, PinOff } from 'lucide-vue-next'

defineProps<{ pinned: boolean }>()
defineEmits<{ toggle: [] }>()

const { t } = useI18n()
</script>

<template>
  <button
    type="button"
    class="flex items-center justify-center h-6 w-6 rounded-md transition-colors cursor-pointer shrink-0"
    :class="pinned
      ? 'bg-primary/15 text-primary hover:bg-primary/25'
      : 'text-foreground/60 hover:text-foreground hover:bg-accent'"
    :title="pinned ? t('item.unpin') : t('item.pin')"
    @click="$emit('toggle')"
  >
    <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
  </button>
</template>
