<script setup lang="ts">
/**
 * The controller's bottom navigation: Inbox · Runs · Todo · Note · Project.
 * Hidden while a session is open (that screen is full height). The Inbox tab
 * carries the count of prompts waiting for an answer.
 */
import { useI18n } from 'vue-i18n'
import { CheckSquare, FileText, Inbox, MessageSquare, Settings2 } from 'lucide-vue-next'

export type ControllerTab = 'inbox' | 'runs' | 'todo' | 'note' | 'project'

defineProps<{
  tab: ControllerTab
  /** Prompts waiting across all runs (the Inbox badge). */
  inboxCount: number
}>()

const emit = defineEmits<{ 'update:tab': [tab: ControllerTab] }>()

const { t } = useI18n()

const TABS = [
  { id: 'inbox', icon: Inbox },
  { id: 'runs', icon: MessageSquare },
  { id: 'todo', icon: CheckSquare },
  { id: 'note', icon: FileText },
  { id: 'project', icon: Settings2 },
] as const
</script>

<template>
  <nav
    class="shrink-0 grid grid-cols-5 border-t border-border bg-card pb-[env(safe-area-inset-bottom)]"
    role="tablist"
  >
    <button
      v-for="item in TABS"
      :key="item.id"
      type="button"
      role="tab"
      :aria-selected="tab === item.id"
      class="relative flex flex-col items-center gap-0.5 pt-2 pb-1.5 text-[10px] font-medium transition-colors cursor-pointer"
      :class="tab === item.id ? 'text-primary' : 'text-foreground/50 hover:text-foreground'"
      @click="emit('update:tab', item.id)"
    >
      <span class="relative">
        <component :is="item.icon" class="h-5 w-5" :stroke-width="2" />
        <span
          v-if="item.id === 'inbox' && inboxCount > 0"
          class="absolute -top-1.5 -right-2.5 min-w-4 h-4 px-1 rounded-full bg-amber-500 text-[10px] leading-4 text-white text-center"
        >
          {{ inboxCount > 99 ? '99+' : inboxCount }}
        </span>
      </span>
      {{ t(`controller.tabs.${item.id}`) }}
    </button>
  </nav>
</template>
