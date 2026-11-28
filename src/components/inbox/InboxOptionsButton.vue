<script setup lang="ts">
// The one settings button of the inbox screens (PR Inbox, GitHub Mentions),
// styled like Delivery Radar's: an icon-only outline button (Settings2) that
// opens a modal holding every filter and view setting — nothing else sits in
// the header. A dot on the button while filters apply. The content comes from
// the screen (default slot, given `close` so an entry that opens another
// dialog can shut this one first); the footer clears the filters / closes.
//
// Section look inside (see RadarSettingsModal): `space-y-5 p-4` body,
// uppercase 11px section titles, 13px row labels with an 11px hint and the
// control on the right, bordered lists and bordered "→" entry buttons.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Modal } from '@/components/ui'
import { Settings2 } from 'lucide-vue-next'

defineProps<{
  /** Filter values that currently apply (dot on the button, "Clear filters" in the footer). */
  activeCount: number
}>()

const emit = defineEmits<{ clear: [] }>()

defineSlots<{ default: (props: { close: () => void }) => unknown }>()

const { t } = useI18n()

const open = ref(false)

function close() {
  open.value = false
}
</script>

<template>
  <Button
    variant="outline"
    size="icon"
    class="relative shrink-0"
    :class="{ 'bg-accent text-foreground': open }"
    aria-haspopup="dialog"
    :aria-expanded="open"
    :title="activeCount > 0 ? `${t('listPane.filterDisplay')} · ${activeCount}` : t('listPane.filterDisplay')"
    :aria-label="t('listPane.filterDisplay')"
    @click="open = true"
  >
    <Settings2 class="h-3.5 w-3.5" :stroke-width="1.75" />
    <span
      v-if="activeCount > 0"
      class="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full bg-primary ring-2 ring-card"
      aria-hidden="true"
    />
  </Button>

  <Modal :open="open" :title="t('listPane.filterDisplay')" size="sm" scroll-body @close="close">
    <div class="space-y-5 p-4">
      <slot :close="close" />
    </div>
    <template #footer>
      <Button v-if="activeCount > 0" variant="ghost" class="mr-auto" @click="emit('clear')">
        {{ t('listPane.clearFilters') }}
      </Button>
      <Button variant="outline" @click="close">{{ t('common.close') }}</Button>
    </template>
  </Modal>
</template>
