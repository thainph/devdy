<script setup lang="ts">
// "+ Todo" button of a source (spec §3.1): Slack thread header, RunView header,
// PR Inbox card. Opens TodoCaptureModal prefilled from the context. `label` is a
// bordered xs header control, `icon` an icon-only button (`buttonVariant` / `size`
// tune it to its row).
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ListPlus } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import type { TodoPrefill } from '@/lib/focus'
import TodoCaptureModal from './TodoCaptureModal.vue'

withDefaults(
  defineProps<{
    prefill?: TodoPrefill
    variant?: 'label' | 'icon'
    buttonVariant?: 'outline' | 'ghost' | 'primary'
    size?: 'xs' | 'sm' | 'icon' | 'icon-sm'
  }>(),
  { prefill: () => ({}), variant: 'label', buttonVariant: 'outline', size: undefined },
)

const { t } = useI18n()
const open = ref(false)
</script>

<template>
  <Button
    v-if="variant === 'icon'"
    :variant="buttonVariant"
    :size="size ?? 'icon'"
    class="shrink-0"
    :title="t('focus.addTodoHint')"
    :aria-label="t('focus.addTodoHint')"
    @click.stop="open = true"
  >
    <ListPlus class="h-4 w-4" :stroke-width="1.75" />
  </Button>
  <Button
    v-else
    :variant="buttonVariant"
    :size="size ?? 'xs'"
    class="shrink-0"
    :title="t('focus.addTodoHint')"
    @click.stop="open = true"
  >
    <ListPlus class="h-3 w-3" :stroke-width="1.75" />
    {{ t('focus.addTodo') }}
  </Button>
  <TodoCaptureModal :open="open" :prefill="prefill" @close="open = false" />
</template>
