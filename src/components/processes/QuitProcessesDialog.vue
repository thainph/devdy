<script setup lang="ts">
// Quit confirmation (main window only). The backend holds the quit and emits
// `app:quit-requested` when sessions are running or processes are left over and
// the policy is 'ask'; every way out of this dialog MUST answer with
// `resolve_quit`, or the app neither quits nor knows it was cancelled. Esc and the
// close button count as Cancel.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { Check } from 'lucide-vue-next'
import { Button, Modal } from '@/components/ui'
import ProcessEntrySummary from './ProcessEntrySummary.vue'
import { useToast } from '@/composables/useToast'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { EVT_QUIT_REQUESTED, type QuitAction, type QuitRequest } from '@/lib/backgroundProcesses'

const { t } = useI18n()
const { toast } = useToast()
const store = useBackgroundProcessesStore()

const request = ref<QuitRequest | null>(null)
const remember = ref(false)
const resolving = ref(false)

const sessions = computed(() => request.value?.entries.filter((e) => e.state === 'active') ?? [])
const strays = computed(() => request.value?.entries.filter((e) => e.state !== 'active') ?? [])
const hasStrays = computed(() => {
  const c = request.value?.counts
  return !!c && c.leftover + c.orphan > 0
})

// Quitting by closing the main window or from the tray may leave it hidden or
// behind other apps; the question must be visible.
async function bringToFront() {
  try {
    const win = getCurrentWebviewWindow()
    await win.unminimize()
    await win.show()
    await win.setFocus()
  } catch {
    /* best-effort focus */
  }
}

let unlisten: UnlistenFn | null = null
onMounted(async () => {
  try {
    unlisten = await listen<QuitRequest>(EVT_QUIT_REQUESTED, (e) => {
      if (!e.payload) return
      // A repeat request while open just refreshes the list.
      if (!request.value) remember.value = false
      request.value = e.payload
      void bringToFront()
    })
  } catch {
    // Outside the Tauri shell (browser dev).
  }
})
onBeforeUnmount(() => {
  unlisten?.()
  unlisten = null
})

async function answer(action: QuitAction) {
  if (!request.value || resolving.value) return
  resolving.value = true
  try {
    await store.resolveQuit(action, action !== 'cancel' && remember.value)
    request.value = null
  } catch (e) {
    toast.error(String(e))
  } finally {
    resolving.value = false
  }
}
</script>

<template>
  <Modal :open="request !== null" :title="t('processes.quit.title')" size="md" :dismiss-on-overlay="false" @close="answer('cancel')">
    <div class="space-y-4 px-4 py-4">
      <div v-if="sessions.length > 0" class="space-y-1.5">
        <p class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('processes.quit.sessions') }} ({{ sessions.length }})
        </p>
        <ProcessEntrySummary :entries="sessions" />
      </div>
      <div v-if="strays.length > 0" class="space-y-1.5">
        <p class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('processes.quit.stray') }} ({{ strays.length }})
        </p>
        <ProcessEntrySummary :entries="strays" />
        <p class="text-[11px] text-muted-foreground">{{ t('processes.quit.strayHint') }}</p>
      </div>
    </div>
    <template #footer>
      <label class="mr-auto flex items-center gap-2 cursor-pointer select-none text-xs text-muted-foreground">
        <span
          class="flex h-4 w-4 shrink-0 items-center justify-center rounded border transition-colors"
          :class="remember ? 'bg-primary border-primary text-white' : 'border-border'"
        >
          <Check v-if="remember" class="h-3 w-3" :stroke-width="3" />
        </span>
        <input v-model="remember" type="checkbox" class="sr-only" />
        {{ t('processes.quit.dontAsk') }}
      </label>
      <Button variant="ghost" :disabled="resolving" @click="answer('cancel')">{{ t('processes.quit.cancel') }}</Button>
      <Button v-if="hasStrays" variant="outline" :disabled="resolving" @click="answer('keep_leftovers')">
        {{ t('processes.quit.keepQuit') }}
      </Button>
      <Button variant="primary" :disabled="resolving" @click="answer('stop_all')">{{ t('processes.quit.stopAllQuit') }}</Button>
    </template>
  </Modal>
</template>
