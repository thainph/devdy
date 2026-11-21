<script setup lang="ts">
// The pop-out window rows of a list sidebar's ⋯ menu (ListSidebarHeader
// `#menu`): keep the window on top, and dock it into the main window as a tab.
// Render it only in a pop-out (not when the screen is a tab of the main window).
import { useI18n } from 'vue-i18n'
import { ArrowDownToLine, Pin, PinOff } from 'lucide-vue-next'
import { DropdownItem, DropdownSeparator } from '@/components/ui'
import ListMenuLabel from './ListMenuLabel.vue'

defineProps<{ pinned: boolean }>()

const emit = defineEmits<{ togglePin: []; dock: [] }>()

const { t } = useI18n()
</script>

<template>
  <DropdownSeparator />
  <ListMenuLabel>{{ t('listPane.ownWindow') }}</ListMenuLabel>
  <DropdownItem :aria-pressed="pinned" @click="emit('togglePin')">
    <component :is="pinned ? PinOff : Pin" class="h-3.5 w-3.5" :stroke-width="1.75" />
    {{ pinned ? t('listPane.unpin') : t('listPane.pin') }}
  </DropdownItem>
  <DropdownItem @click="emit('dock')">
    <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />
    {{ t('common.dockIntoMain') }}
  </DropdownItem>
</template>
