<script setup lang="ts">
// The ⋯ menu that ends the header of the Slack / Notes / Todos windows' detail
// pane: New session from this … (disabled without a project) · Copy ID ·
// host-specific items (default slot) · Delete. The host confirms deletes and
// shows the toasts.
import { useI18n } from 'vue-i18n'
import { Check, Copy, MoreHorizontal, Plus, Trash2 } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'

defineProps<{
  /** "New session from this thread / note / todo". */
  newSessionLabel: string
  /** Its tooltip; with no project, the "pick a project first" hint instead. */
  newSessionHint: string
  /** false disables New session (no project). */
  canNewSession: boolean
  /** Copy ID just fired: its icon becomes a tick. */
  copiedId: boolean
}>()

const emit = defineEmits<{ newSession: []; copyId: []; delete: [] }>()

const { t } = useI18n()
</script>

<template>
  <DropdownMenu align="right" class="shrink-0">
    <template #trigger>
      <Button
        variant="outline"
        size="icon-sm"
        :title="t('item.moreActions')"
        :aria-label="t('item.moreActions')"
      >
        <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </template>
    <DropdownItem :disabled="!canNewSession" :title="newSessionHint" @click="emit('newSession')">
      <Plus class="h-3.5 w-3.5" :stroke-width="1.75" />
      {{ newSessionLabel }}
    </DropdownItem>
    <!-- Label stays put: the tick and the toast carry the feedback. -->
    <DropdownItem @click="emit('copyId')">
      <component :is="copiedId ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
      {{ t('item.copyId') }}
    </DropdownItem>
    <slot />
    <DropdownSeparator />
    <DropdownItem variant="destructive" @click="emit('delete')">
      <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
      {{ t('common.delete') }}
    </DropdownItem>
  </DropdownMenu>
</template>
