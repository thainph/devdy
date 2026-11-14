<script setup lang="ts">
// The Batch Board queue: todos added from the todo screen ("Add to Batch
// Board"). Pick which to run and how (tasks at once, how workers keep apart,
// permissions, engine), then start one Todo Conductor on them.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ListTodo, Play, X } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { AppSelect, Badge, Banner, Button, Card, Checkbox, SegmentedControl } from '@/components/ui'
import type { QueuedTodo } from '@/lib/batchBoard'

const props = defineProps<{ queue: QueuedTodo[] }>()
const emit = defineEmits<{
  /** A batch started on the picked todos (its conductor run id). */
  started: [runId: string]
  /** A todo was removed from the queue. */
  changed: []
}>()

const { t } = useI18n()

const picked = ref(new Set<string>())
const parallel = ref('3')
/** How workers keep their changes apart; `rules` works for any project layout. */
const isolation = ref<'rules' | 'devdy' | 'none'>('rules')
const permissionMode = ref('')
const engine = ref('')
const starting = ref(false)
const error = ref<string | null>(null)

// Newly queued todos start picked; ones that left the queue are dropped.
let known = new Set<string>()
watch(
  () => props.queue.map((q) => q.todo.id),
  (ids) => {
    const next = new Set([...picked.value].filter((id) => ids.includes(id)))
    for (const id of ids) if (!known.has(id)) next.add(id)
    known = new Set(ids)
    picked.value = next
  },
  { immediate: true },
)

const allState = computed<boolean | 'mixed'>(() =>
  picked.value.size === 0 ? false : picked.value.size === props.queue.length ? true : 'mixed',
)
function toggle(id: string) {
  const next = new Set(picked.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  picked.value = next
}
function toggleAll() {
  picked.value = allState.value === true ? new Set() : new Set(props.queue.map((q) => q.todo.id))
}

async function remove(todoId: string) {
  try {
    await invoke('board_queue_remove', { todoId })
    emit('changed')
  } catch (e) {
    error.value = String(e)
  }
}

const parallelOptions = ['1', '2', '3', '5', '8'].map((v) => ({ value: v, label: v }))
const isolationOptions = computed(() => [
  { value: 'rules' as const, label: t('board.start.isolationRules') },
  { value: 'devdy' as const, label: t('board.start.isolationDevdy') },
  { value: 'none' as const, label: t('board.start.isolationNone') },
])
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
      todoIds: props.queue.filter((q) => picked.value.has(q.todo.id)).map((q) => q.todo.id),
      homeProjectId: null,
      maxParallel: Number(parallel.value),
      isolation: isolation.value,
      engine: engine.value || null,
      permissionMode: permissionMode.value || null,
    })
    emit('started', runId)
  } catch (e) {
    error.value = String(e)
  } finally {
    starting.value = false
  }
}
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <template #header>
      <ListTodo class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('board.queue.title') }}</span>
      <span class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none">
        {{ queue.length }}
      </span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('board.start.hint') }}</p>

    <div class="space-y-1">
      <Checkbox :model-value="allState" @update:model-value="toggleAll">
        <span class="text-xs font-medium">{{ t('board.start.pickAll', { n: picked.size, total: queue.length }) }}</span>
      </Checkbox>
      <div class="max-h-64 overflow-auto rounded-md border border-border divide-y divide-border/50">
        <div v-for="q in queue" :key="q.todo.id" class="group flex items-center gap-2 pl-3 pr-1 py-1">
          <Checkbox class="min-w-0 flex-1" :model-value="picked.has(q.todo.id)" @update:model-value="toggle(q.todo.id)">
            <span class="flex min-w-0 items-center gap-2">
              <span class="truncate text-xs">{{ q.todo.text.split('\n')[0] || t('board.untitled') }}</span>
              <Badge v-if="q.projectName" size="xs" tone="neutral" class="shrink-0">{{ q.projectName }}</Badge>
            </span>
          </Checkbox>
          <Button
            variant="ghost"
            size="icon-xs"
            class="shrink-0"
            :title="t('board.queue.remove')"
            :aria-label="t('board.queue.remove')"
            @click="remove(q.todo.id)"
          >
            <X class="h-3 w-3" :stroke-width="2" />
          </Button>
        </div>
      </div>
    </div>

    <div class="grid gap-3 sm:grid-cols-2">
      <div class="space-y-1.5">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.start.parallel') }}</label>
        <SegmentedControl v-model="parallel" :options="parallelOptions" />
      </div>
      <div class="space-y-1.5">
        <label class="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{{ t('board.start.isolation') }}</label>
        <SegmentedControl v-model="isolation" :options="isolationOptions" />
      </div>
      <p class="text-[11px] text-muted-foreground sm:col-span-2">{{ t(`board.start.isolationHint.${isolation}`) }}</p>
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

    <div class="flex justify-end">
      <Button size="sm" :disabled="!picked.size || starting" @click="start">
        <Play class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ starting ? t('board.start.starting') : t('board.start.submit', { n: picked.size }) }}
      </Button>
    </div>
  </Card>
</template>
