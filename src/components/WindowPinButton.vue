<script setup lang="ts">
// The pin toggle of every pop-out window (item, capture): pinned keeps the window
// above other apps even after it loses focus (see composables/useFloatingWindow).
// The host owns the state, since useFloatingWindow must run once per window.
import { useI18n } from 'vue-i18n'
import { Pin, PinOff } from 'lucide-vue-next'
import { Button } from '@/components/ui'

defineProps<{ pinned: boolean }>()
defineEmits<{ toggle: [] }>()

const { t } = useI18n()
</script>

<template>
  <Button
    :variant="pinned ? 'primary' : 'outline'"
    size="icon-sm"
    class="shrink-0"
    :aria-pressed="pinned"
    :title="pinned ? t('item.unpin') : t('item.pin')"
    :aria-label="pinned ? t('item.unpin') : t('item.pin')"
    @click="$emit('toggle')"
  >
    <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
  </Button>
</template>
