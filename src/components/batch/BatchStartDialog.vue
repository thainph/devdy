<script setup lang="ts">
// "Hand to AI": pick open todos (any projects) and start one Todo Conductor
// session that works through them, then open the Batch Board on it.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit as emitEvent } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { AppSelect, Banner, Button, Checkbox, Modal, SegmentedControl, ToggleSwitch } from '@/components/ui'
import { BOARD_OPEN_ROUTE, boardPath, type BatchCandidate } from '@/lib/batchBoard'

const props = defineProps<{
  open: boolean
  candidates: BatchCandidate[]
  projectNames: Record<string, string>
}>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()

const picked = ref(new Set<string>())
const parallel = ref('3')
const worktree = ref(true)
const permissionMode = ref('')
const engine = ref('')
const starting = ref(false)
const error = ref<string | null>(null)

// Every todo with a project starts picked.
watch(
  () => props.open,
  (open) => {
    if (!open) return
    picked.value = new Set(props.candidates.filter((c) => c.projectId).map((c) => c.id))
    error.value = null
  },
  { immediate: true },
)

const pickable = computed(() => props.candidates.filter((c) => c.projectId))
const allState = computed<boolean | 'mixed'>(() =>
  picked.value.size === 0 ? false : picked.value.size === pickable.value.length ? true : 'mixed',
)

function toggle(id: string) {
  const next = new Set(picked.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  picked.value = next
}
function toggleAll() {
  picked.value = allState.value === true ? new Set() : new Set(pickable.value.map((c) => c.id))
}

const parallelOptions = ['1', '2', '3', '5', '8'].map((v) => ({ value: v, label: v }))
const permissionOptions = computed(() => [
  { value: '', label: t('board.start.permissionDefault') },
  { value: 'acceptEdits', label: t('board.start.permissionAcceptEdits') },
  { value: 'plan', label: t('board.start.permissionPlan') },
  { value: 'bypassPermissions', label: t('board.start.permissionBypass') },
])
const engineOptions = computed(() => [
  { value: '', label: t('board.start.engineDefault') },
  { value: 'claude', label: 'Claude' },
  { value: 'codex', label: 'Codex' },
])

async function start() {
  if (!picked.value.size || starting.value) return
  starting.value = true
  error.value = null
  try {
    const runId = await invoke<string>('todo_batch_start', {
      todoIds: props.candidates.filter((c) => picked.value.has(c.id)).map((c) => c.id),
      homeProjectId: null,
      maxParallel: Number(parallel.value),
      worktree: worktree.value,
      engine: engine.value || null,
      permissionMode: permissionMode.value || null,
    })
    await emitEvent(BOARD_OPEN_ROUTE, { path: boardPath(runId) })
    emit('close')
  } catch (e) {
    error.value = String(e)
  } finally {
    starting.value = false
  }
}
</script>

<template>
  <Modal :open="open" :title="t('board.start.title')" size="lg" scroll-body @close="emit('close')">
    <div class="px-4 py-4 space-y-4">
      <p class="text-xs text-muted-foreground">{{ t('board.start.hint') }}</p>

      <div class="space-y-1">
        <Checkbox :model-value="allState" @update:model-value="toggleAll">
          <span class="text-xs font-medium">{{ t('board.start.pickAll', { n: picked.size, total: candidates.length }) }}</span>
        </Checkbox>
        <div class="max-h-64 overflow-auto rounded-md border border-border divide-y divide-border/50">
          <div v-for="c in candidates" :key="c.id" class="px-3 py-1.5">
            <Checkbox
              :model-value="picked.has(c.id)"
              :disabled="!c.projectId"
              @update:model-value="toggle(c.id)"
            >
              <span class="flex min-w-0 items-center gap-2">
                <span class="truncate text-xs">{{ c.title || t('board.untitled') }}</span>
                <span v-if="c.projectId" class="shrink-0 text-[11px] text-muted-foreground">{{ projectNames[c.projectId] ?? '' }}</span>
                <span v-else class="shrink-0 text-[11px] text-warning">{{ t('board.start.noProject') }}</span>
              </span>
            </Checkbox>
          </div>
          <p v-if="!candidates.length" class="px-3 py-4 text-center text-[11px] text-muted-foreground">{{ t('board.start.none') }}</p>
        </div>
      </div>

      <div class="grid gap-3 sm:grid-cols-2">
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.start.parallel') }}</label>
          <SegmentedControl v-model="parallel" :options="parallelOptions" />
        </div>
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.start.worktree') }}</label>
          <div class="flex h-8 items-center gap-2">
            <ToggleSwitch :checked="worktree" size="md" @toggle="worktree = !worktree" />
            <span class="text-[11px] text-muted-foreground">{{ worktree ? t('board.start.worktreeOn') : t('board.start.worktreeOff') }}</span>
          </div>
        </div>
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.start.permission') }}</label>
          <AppSelect v-model="permissionMode" :options="permissionOptions" />
        </div>
        <div class="space-y-1.5">
          <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.start.engine') }}</label>
          <AppSelect v-model="engine" :options="engineOptions" />
        </div>
      </div>

      <Banner v-if="error" tone="destructive">{{ error }}</Banner>
    </div>
    <template #footer>
      <Button variant="ghost" size="sm" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button size="sm" :disabled="!picked.size || starting" @click="start">
        {{ starting ? t('board.start.starting') : t('board.start.submit', { n: picked.size }) }}
      </Button>
    </template>
  </Modal>
</template>
