<script setup lang="ts">
// Top row of a list-detail sidebar (Todos / Notes, Slack threads, Skills,
// Rules, MCP, Servers, a run's History):
//
//   Title 12 [badge]                         [+ New] [⋯]
//
// - `count`: shown next to the title (tabular, muted); omit to hide.
// - `#badge`: a quick filter beside the count (the red "3 late" of Todos).
// - `newLabel`: shows the primary "+ New" button (emits `new`); `newTitle` is
//   its tooltip (e.g. with the shortcut).
// - `#menu`: rows of the ⋯ menu (DropdownItem / DropdownSeparator /
//   ListMenuLabel). Under a screen that provides a list pane
//   (composables/useListPane) the menu always ends with "Hide the list (⌘B)";
//   while the list floats as a peek, a ✕ closing it replaces the ⋯ instead.
// - `#actions`: extra controls placed before "+ New" (rarely needed).
// - `closable`: the screen floats the list its own way (RunView's History over
//   a narrow body): a ✕ emitting `close` follows the ⋯, which stays.
import { computed, useSlots } from 'vue'
import { useI18n } from 'vue-i18n'
import { MoreHorizontal, PanelLeftClose, Plus, X } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
import { useListPane } from '@/composables/useListPane'
import { IS_MAC } from '@/lib/appMenu'

defineProps<{
  title: string
  count?: number | null
  /** Label of the primary "+ New" button; omit for none. */
  newLabel?: string
  /** Tooltip of "+ New" (defaults to the label). */
  newTitle?: string
  /** "+ New" is busy (e.g. creating). */
  newDisabled?: boolean
  /** Show a ✕ (emits `close`) after the ⋯ menu. */
  closable?: boolean
}>()

const emit = defineEmits<{ new: []; close: [] }>()

const { t } = useI18n()
const slots = useSlots()
const pane = useListPane()
const mod = IS_MAC ? '⌘' : 'Ctrl+'

/** The hide item only while the list is docked (a peek closes with ✕). */
const canHide = computed(() => !!pane && !pane.hidden.value)
const showMenu = computed(() => !!slots.menu || canHide.value)
</script>

<template>
  <div class="flex shrink-0 items-center gap-1.5 pb-1.5 pl-4 pr-2.5 pt-3">
    <h2 class="min-w-0 truncate text-sm font-bold">{{ title }}</h2>
    <span v-if="count != null" class="shrink-0 text-[11px] tabular-nums text-muted-foreground">{{ count }}</span>
    <slot name="badge" />
    <span class="min-w-0 flex-1" />
    <slot name="actions" />
    <Button v-if="newLabel" size="sm" class="shrink-0 pl-2.5" :disabled="newDisabled" :title="newTitle ?? newLabel" @click="emit('new')">
      <Plus class="h-3.5 w-3.5" :stroke-width="2" />
      {{ newLabel }}
    </Button>
    <Button
      v-if="pane?.peeking.value"
      variant="ghost"
      size="icon"
      class="shrink-0"
      :title="t('listPane.closePeek')"
      :aria-label="t('listPane.closePeek')"
      @click="pane.closePeek()"
    >
      <X class="h-3.5 w-3.5" :stroke-width="1.75" />
    </Button>
    <DropdownMenu v-else-if="showMenu" align="right" class="shrink-0">
      <template #trigger="{ open }">
        <Button
          variant="ghost"
          size="icon"
          :class="open && 'bg-accent text-foreground'"
          :title="t('listPane.more')"
          :aria-label="t('listPane.more')"
        >
          <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </template>
      <slot name="menu" />
      <template v-if="canHide">
        <DropdownSeparator v-if="$slots.menu" />
        <DropdownItem @click="pane?.hide()">
          <PanelLeftClose class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('listPane.hide') }}
          <kbd class="ml-auto pl-4 font-mono text-[10px] text-muted-foreground">{{ mod }}B</kbd>
        </DropdownItem>
      </template>
    </DropdownMenu>
    <Button
      v-if="closable"
      variant="ghost"
      size="icon"
      class="shrink-0"
      :title="t('listPane.closePeek')"
      :aria-label="t('listPane.closePeek')"
      @click="emit('close')"
    >
      <X class="h-3.5 w-3.5" :stroke-width="1.75" />
    </Button>
  </div>
</template>
