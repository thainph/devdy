<script setup lang="ts">
// Floating "quick create" action button, pinned to the bottom-right corner and
// visible on every screen (mounted in App.vue). Click to expand a small
// speed-dial with Todo / Note; each opens the standalone, always-on-top
// quick-create OS window (see openQuickCreateWindow) so you can jot something
// on a second monitor without any overlay covering the current view.
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Plus, ListTodo, StickyNote } from 'lucide-vue-next'
import { openQuickCreateWindow, type QuickCreateTab } from '@/lib/quickCreateWindow'

const { t } = useI18n()
const expanded = ref(false)
const rootRef = ref<HTMLElement | null>(null)

function toggle() {
  expanded.value = !expanded.value
}

function pick(tab: QuickCreateTab) {
  expanded.value = false
  openQuickCreateWindow(tab)
}

function onClickOutside(e: MouseEvent) {
  if (expanded.value && !rootRef.value?.contains(e.target as Node)) expanded.value = false
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') expanded.value = false
  // Global shortcut: Cmd/Ctrl+K opens the quick-create window (Todo tab).
  if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
    e.preventDefault()
    openQuickCreateWindow('todo')
  }
}

onMounted(() => {
  document.addEventListener('mousedown', onClickOutside)
  window.addEventListener('keydown', onKey)
})
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onClickOutside)
  window.removeEventListener('keydown', onKey)
})

const actions: { key: QuickCreateTab; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'note', labelKey: 'todos.quick.actionNote', icon: StickyNote },
  { key: 'todo', labelKey: 'todos.quick.actionTodo', icon: ListTodo },
]
</script>

<template>
  <div ref="rootRef" class="fixed bottom-5 right-5 z-40 flex flex-col items-end gap-2">
    <!-- Speed-dial actions -->
    <Transition
      enter-active-class="transition duration-150 ease-out"
      enter-from-class="opacity-0 translate-y-1"
      enter-to-class="opacity-100 translate-y-0"
      leave-active-class="transition duration-100 ease-in"
      leave-from-class="opacity-100 translate-y-0"
      leave-to-class="opacity-0 translate-y-1"
    >
      <div v-if="expanded" class="flex flex-col items-end gap-2">
        <button
          v-for="a in actions"
          :key="a.key"
          type="button"
          class="inline-flex items-center gap-2 rounded-full border border-border/70 bg-popover pl-3 pr-4 py-2 text-sm font-medium text-foreground shadow-lg shadow-black/20 transition-colors cursor-pointer hover:bg-accent"
          @click="pick(a.key)"
        >
          <component :is="a.icon" class="h-4 w-4 text-primary" :stroke-width="1.75" />
          {{ t(a.labelKey) }}
        </button>
      </div>
    </Transition>

    <!-- Main FAB -->
    <button
      type="button"
      :aria-label="t('todos.quick.fabAriaLabel')"
      :title="t('todos.quick.fabTitle')"
      class="flex h-12 w-12 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg shadow-primary/30 transition-transform cursor-pointer hover:opacity-90 active:scale-95"
      @click="toggle"
    >
      <Plus class="h-5 w-5 transition-transform duration-200" :class="expanded && 'rotate-45'" :stroke-width="2" />
    </button>
  </div>
</template>
