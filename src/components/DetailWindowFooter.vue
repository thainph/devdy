<script setup lang="ts">
// The action bar shared by the detail pop-outs — note / todo (views/ItemWindow.vue)
// and Slack thread / web page (views/CaptureWindow.vue) — so all four read alike:
//
//   reading  [left: project] ··········· [actions: the kind's own] [Edit] [⋯]
//   editing  ················································ [Cancel] [Save]
//
// Every action sits on the right; the left holds the item's project only while
// reading (the note / todo edit form has its own project field). Edit and Save
// share one slot, so the bar never offers both at once. Delete always lives in
// the ⋯ menu (Copy ID · Delete), behind the host's confirm; the `menu` slot adds
// kind-specific items above Copy ID. A host with a different job (the item
// window's create mode) passes the default slot and gets only the bar.
import { useI18n } from 'vue-i18n'
import { Check, Copy, MoreHorizontal, Pencil, Trash2 } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'

withDefaults(
  defineProps<{
    editing?: boolean
    /** Save is offered but disabled while false. */
    canSave?: boolean
    saveLabel?: string
    /** Copy ID just fired: its icon becomes a tick. */
    copiedId?: boolean
  }>(),
  { editing: false, canSave: true, saveLabel: undefined, copiedId: false },
)

defineEmits<{ edit: []; cancel: []; save: []; copyId: []; delete: [] }>()

const { t } = useI18n()
</script>

<template>
  <div class="flex items-center gap-2 border-t border-border/60 px-3 py-2.5 shrink-0">
    <slot>
      <template v-if="editing">
        <span class="mr-auto" />
        <Button variant="ghost" size="sm" @click="$emit('cancel')">
          {{ t('common.cancel') }}
        </Button>
        <Button variant="primary" size="sm" :disabled="!canSave" @click="$emit('save')">
          {{ saveLabel ?? t('common.save') }}
        </Button>
      </template>
      <template v-else>
        <div class="mr-auto w-44 min-w-0 shrink">
          <slot name="left" />
        </div>
        <slot name="actions" />
        <Button variant="primary" size="sm" :title="t('item.editTitle')" @click="$emit('edit')">
          <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('item.edit') }}
        </Button>
        <DropdownMenu align="right">
          <template #trigger>
            <Button variant="ghost" size="icon" :title="t('item.moreActions')">
              <MoreHorizontal class="h-4 w-4" :stroke-width="1.75" />
            </Button>
          </template>
          <slot name="menu" />
          <!-- Label stays put: the tick and the toast carry the feedback. -->
          <DropdownItem @click="$emit('copyId')">
            <component :is="copiedId ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('item.copyId') }}
          </DropdownItem>
          <DropdownSeparator />
          <DropdownItem variant="destructive" @click="$emit('delete')">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.delete') }}
          </DropdownItem>
        </DropdownMenu>
      </template>
    </slot>
  </div>
</template>
