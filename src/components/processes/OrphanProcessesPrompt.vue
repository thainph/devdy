<script setup lang="ts">
// Startup prompt (main window, once per launch): processes left running by the
// previous app session. Waits for the store's first snapshot, then offers to stop
// them, review them on the Processes screen, or leave them be.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { AlertTriangle } from 'lucide-vue-next'
import { Button, Modal } from '@/components/ui'
import ProcessEntrySummary from './ProcessEntrySummary.vue'
import { useToast } from '@/composables/useToast'
import { claimOrphanPrompt, useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import type { BgEntry } from '@/lib/backgroundProcesses'

const { t } = useI18n()
const router = useRouter()
const { toast } = useToast()
const store = useBackgroundProcessesStore()

const open = ref(false)
const stopping = ref(false)
// Frozen at prompt time: the list must not reshuffle under the user's cursor.
const orphans = ref<BgEntry[]>([])
const processCount = computed(() => orphans.value.reduce((n, e) => n + e.processes.length, 0))

onMounted(async () => {
  await store.start()
  if (store.counts.orphan <= 0) return
  const list = (store.snapshot?.entries ?? []).filter((e) => e.state === 'orphan')
  if (!list.length || !claimOrphanPrompt()) return
  orphans.value = list
  open.value = true
})

async function stopAll() {
  stopping.value = true
  try {
    const n = await store.stopEntries(orphans.value.map((e) => e.id))
    toast.success(t('processes.toast.stopped', { n }, n))
    open.value = false
  } catch (e) {
    toast.error(t('processes.toast.failed', { error: String(e) }))
  } finally {
    stopping.value = false
  }
}

function viewDetails() {
  open.value = false
  router.push('/processes').catch(() => {})
}
</script>

<template>
  <Modal :open="open" size="md" @close="open = false">
    <template #header>
      <AlertTriangle class="h-4 w-4 shrink-0 text-amber-500" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">
        {{ t('processes.orphanPrompt.title', { n: processCount }, processCount) }}
      </h3>
    </template>
    <div class="space-y-3 px-4 py-4">
      <p class="text-sm text-muted-foreground">{{ t('processes.orphanPrompt.message') }}</p>
      <ProcessEntrySummary :entries="orphans" />
    </div>
    <template #footer>
      <Button variant="ghost" @click="open = false">{{ t('processes.orphanPrompt.leaveRunning') }}</Button>
      <Button variant="outline" @click="viewDetails">{{ t('processes.orphanPrompt.viewDetails') }}</Button>
      <Button variant="primary" :disabled="stopping" @click="stopAll">{{ t('processes.orphanPrompt.stopAll') }}</Button>
    </template>
  </Modal>
</template>
