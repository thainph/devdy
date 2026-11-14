<script setup lang="ts">
// A session on the board is blocked on the user: one line saying what it asks,
// clicking it opens the prompt in the board's "Waiting for you" panel (the
// prompt itself is only shown there, see BatchInbox).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronRight, ClipboardList, HelpCircle, ShieldAlert } from 'lucide-vue-next'
import { pendingSummary, type PendingLike } from '@/lib/batchBoard'

const props = defineProps<{ request: PendingLike; more?: number }>()
const emit = defineEmits<{ answer: [] }>()

const { t } = useI18n()
const s = computed(() => pendingSummary(props.request, 120))
const icon = computed(() => ({ permission: ShieldAlert, question: HelpCircle, plan: ClipboardList })[s.value.kind])
const tone = computed(() =>
  s.value.kind === 'permission'
    ? 'border-warning/30 bg-warning/10 text-warning hover:bg-warning/15'
    : 'border-info/30 bg-info/10 text-info hover:bg-info/15',
)
</script>

<template>
  <button
    type="button"
    class="flex w-full items-center gap-2 rounded-md border px-2.5 py-1.5 text-left transition-colors cursor-pointer"
    :class="tone"
    :title="t('board.inbox.answerTitle')"
    @click="emit('answer')"
  >
    <component :is="icon" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
    <span class="min-w-0 flex-1 truncate text-[11px] text-foreground">
      <template v-if="s.kind === 'permission'">
        <span class="font-mono">{{ s.tool }}</span><template v-if="s.detail"> · <span class="font-mono text-muted-foreground">{{ s.detail }}</span></template>
      </template>
      <template v-else>
        {{ t(`board.inbox.${s.kind}`) }}<template v-if="s.detail">: <span class="text-muted-foreground">{{ s.detail }}</span></template>
      </template>
    </span>
    <span v-if="more" class="shrink-0 rounded-full bg-muted px-1.5 text-[10px] tabular-nums text-muted-foreground">+{{ more }}</span>
    <span class="flex shrink-0 items-center text-[10px] font-medium">
      {{ t('board.inbox.answer') }}<ChevronRight class="h-3 w-3" :stroke-width="2" />
    </span>
  </button>
</template>
