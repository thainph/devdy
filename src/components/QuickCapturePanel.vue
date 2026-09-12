<script setup lang="ts">
// In-app quick-capture surface: a right-side slide-over that jots down a Todo or
// Note WITHOUT leaving the current screen.
//
// This exists because navigating to /todos or /notes remounts the run workspace
// (App.vue keys RouterView per project and there is no KeepAlive), which throws
// away scroll position and open panels mid-session. An overlay costs none of
// that: Esc puts the user right back where they were, and the draft is kept in
// useQuickCapture so reopening resumes exactly where they stopped.
//
// Mounted once in App.vue and driven entirely through useQuickCapture, so the
// ⌘K / ⌘⇧N shortcuts, the mascot menu and the "save selection" button in a run
// all open this one panel.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ListTodo, StickyNote, AppWindow } from 'lucide-vue-next'
import { Button, Drawer } from '@/components/ui'
import QuickCaptureForm from '@/components/QuickCaptureForm.vue'
import { useQuickCapture, type QuickCaptureTab } from '@/composables/useQuickCapture'
import { useProjectsStore } from '@/stores/projects'
import { openQuickCreateWindow } from '@/lib/quickCreateWindow'

const { t } = useI18n()
const projects = useProjectsStore()
const { open, tab, projectId, runId, closeCapture } = useQuickCapture()

const projectLabel = computed(() => {
  if (!projectId.value) return null
  return projects.projects.find((p) => p.id === projectId.value)?.name ?? null
})

const TABS: { key: QuickCaptureTab; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'todo', labelKey: 'todos.quick.tabTodo', icon: ListTodo },
  { key: 'note', labelKey: 'todos.quick.tabNote', icon: StickyNote },
]

const formRef = ref<InstanceType<typeof QuickCaptureForm> | null>(null)

// Saving keeps the panel open so several thoughts can be captured in a row; the
// user dismisses with Esc / Cancel when they're done.

/** Hand the capture over to the standalone always-on-top window instead. */
function popOut() {
  openQuickCreateWindow(tab.value, { projectId: projectId.value || null, runId: runId.value })
  closeCapture()
}
</script>

<template>
  <!-- No overlay dismissal: a stray click must not drop a half-typed capture. -->
  <Drawer :open="open" side="right" size="lg" :dismiss-on-overlay="false" @close="closeCapture">
    <template #header>
      <div class="flex flex-1 items-center gap-1 min-w-0">
        <button
          v-for="item in TABS"
          :key="item.key"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors cursor-pointer"
          :class="tab === item.key
            ? 'bg-accent text-foreground'
            : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
          @click="tab = item.key"
        >
          <component :is="item.icon" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t(item.labelKey) }}
        </button>
        <span
          v-if="projectLabel"
          class="ml-1 truncate text-[11px] text-muted-foreground"
          :title="projectLabel"
        >
          · {{ projectLabel }}
        </span>
      </div>
      <button
        type="button"
        class="flex items-center gap-1 rounded px-1.5 py-1 text-[11px] text-muted-foreground hover:bg-accent/60 hover:text-foreground transition-colors cursor-pointer"
        :title="t('todos.quick.popOutTitle')"
        @click="popOut"
      >
        <AppWindow class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('todos.quick.popOut') }}
      </button>
    </template>

    <div class="p-5">
      <QuickCaptureForm v-if="open" ref="formRef" @dismiss="closeCapture" />
      <p class="mt-3 text-[11px] text-muted-foreground">
        {{ t('todos.quick.panelHint') }}
      </p>
    </div>

    <template #footer>
      <div class="flex flex-1 items-center justify-end gap-2">
        <Button variant="ghost" size="sm" @click="closeCapture">{{ t('common.cancel') }}</Button>
        <Button
          size="sm"
          :disabled="!formRef?.canSubmit || formRef?.saving"
          @click="formRef?.submit()"
        >
          {{ tab === 'todo' ? t('todos.quick.addTodo') : t('todos.quick.saveNote') }}
        </Button>
      </div>
    </template>
  </Drawer>
</template>
