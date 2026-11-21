<script setup lang="ts">
// Buttons of the hideable list pane (composables/useListPane):
//  - `mode="hide"` (in the list's own header): ⇤ hide the list — or, while it
//    floats as a peek, ✕ close the peek.
//  - `mode="show"` (in the detail header / empty state): while the list is
//    hidden, ⇥ show it again and ☰ peek at it — only ☰ while the screen is
//    too narrow to dock it (useListPane `autoHide`).
// Renders nothing outside a screen that provides a pane.
import { useI18n } from 'vue-i18n'
import { List, PanelLeftClose, PanelLeftOpen, X } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { useListPane } from '@/composables/useListPane'
import { IS_MAC } from '@/lib/appMenu'

defineProps<{ mode: 'hide' | 'show' }>()

const { t } = useI18n()
const pane = useListPane()
const mod = IS_MAC ? '⌘' : 'Ctrl+'
</script>

<template>
  <template v-if="pane">
    <template v-if="mode === 'hide'">
      <Button
        v-if="pane.peeking.value"
        variant="ghost"
        size="icon"
        class="shrink-0"
        :title="t('listPane.closePeek')"
        :aria-label="t('listPane.closePeek')"
        @click="pane.closePeek()"
      >
        <X class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        v-else
        variant="outline"
        size="icon"
        class="shrink-0"
        :title="`${t('listPane.hide')} (${mod}B)`"
        :aria-label="t('listPane.hide')"
        @click="pane.hide()"
      >
        <PanelLeftClose class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </template>
    <div v-else-if="pane.hidden.value" class="flex shrink-0 items-center gap-1">
      <Button
        v-if="!pane.narrow.value"
        variant="outline"
        size="icon"
        :title="`${t('listPane.show')} (${mod}B)`"
        :aria-label="t('listPane.show')"
        @click="pane.show()"
      >
        <PanelLeftOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        :variant="pane.peeking.value ? 'primary' : 'outline'"
        size="icon"
        data-list-peek
        :aria-pressed="pane.peeking.value"
        :title="pane.narrow.value ? `${t('listPane.peek')} (${mod}B)` : t('listPane.peek')"
        :aria-label="t('listPane.peek')"
        @click="pane.togglePeek()"
      >
        <List class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </div>
  </template>
</template>
