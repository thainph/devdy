<script setup lang="ts">
// The Batch Board queue: todos added from the todo screen ("Add to Batch
// Board"), grouped by project. Run starts one Todo Conductor per project, living
// in that project — its workers never leave it (backend `board_run`). How the
// workers run (tasks at once, isolation, permissions, engine) is tucked under
// "Options".
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ChevronRight, Folder, ListTodo, Play, X } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { AppSelect, Banner, Button, Card, Checkbox, SegmentedControl } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { groupQueueByProject, type BoardRunResult, type QueuedTodo } from '@/lib/batchBoard'

const props = defineProps<{ queue: QueuedTodo[] }>()
const emit = defineEmits<{
  /** Conductors started (one per project). */
  started: [result: BoardRunResult]
  /** A todo was removed from the queue. */
  changed: []
}>()

const { t } = useI18n()
const { toast } = useToast()

const groups = computed(() => groupQueueByProject(props.queue))
const picked = ref(new Set<string>())
const showOptions = ref(false)
const parallel = ref('3')
/** How workers keep their changes apart; `rules` works for any project layout. */
const isolation = ref<'rules' | 'devdy' | 'none'>('rules')
const permissionMode = ref('')
const engine = ref('')
const starting = ref(false)
const error = ref<string | null>(null)

// Newly queued todos start picked (each project runs on its own, so nothing
// gets mixed); ones that left the queue are dropped.
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

const pickedProjects = computed(() => groups.value.filter((g) => g.todos.some((q) => picked.value.has(q.todo.id))).length)

function groupState(ids: string[]): boolean | 'mixed' {
  const n = ids.filter((id) => picked.value.has(id)).length
  return n === 0 ? false : n === ids.length ? true : 'mixed'
}
function toggle(id: string) {
  const next = new Set(picked.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  picked.value = next
}
function toggleGroup(ids: string[]) {
  const next = new Set(picked.value)
  const all = groupState(ids) === true
  for (const id of ids) {
    if (all) next.delete(id)
    else next.add(id)
  }
  picked.value = next
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
    const result = await invoke<BoardRunResult>('board_run', {
      todoIds: props.queue.filter((q) => picked.value.has(q.todo.id)).map((q) => q.todo.id),
      options: {
        maxParallel: Number(parallel.value),
        isolation: isolation.value,
        engine: engine.value || null,
        permissionMode: permissionMode.value || null,
      },
    })
    if (result.failed.length) {
      error.value = result.failed.map((f) => `${f.projectName}: ${f.error}`).join('\n')
    }
    if (result.skipped.length) {
      const byReason = new Map<string, number>()
      for (const sk of result.skipped) byReason.set(sk.reason, (byReason.get(sk.reason) ?? 0) + 1)
      const why = [...byReason].map(([r, n]) => t(`board.start.skippedReason.${r}`, { n }, n)).join(' · ')
      toast.info(`${t('board.start.skipped', { n: result.skipped.length }, result.skipped.length)}: ${why}`, 6000)
    }
    if (result.started.length) {
      toast.success(t('board.start.started', { n: result.started.length }, result.started.length))
      emit('started', result)
    }
  } catch (e) {
    error.value = String(e)
  } finally {
    starting.value = false
  }
}
</script>

<template>
  <Card body-class="p-4 space-y-3">
    <template #header>
      <ListTodo class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('board.queue.title') }}</span>
      <span class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none">
        {{ queue.length }}
      </span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('board.start.hint') }}</p>

    <!-- One block per project: that's one conductor. -->
    <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
      <div v-for="g in groups" :key="g.projectId" class="rounded-md border border-border">
        <div class="flex items-center gap-2 border-b border-border/60 bg-muted/30 px-3 py-1.5">
          <Checkbox
            :model-value="groupState(g.todos.map((q) => q.todo.id))"
            @update:model-value="toggleGroup(g.todos.map((q) => q.todo.id))"
          >
            <span class="flex min-w-0 items-center gap-1.5 text-xs font-semibold">
              <Folder class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <span class="truncate">{{ g.projectName }}</span>
            </span>
          </Checkbox>
          <span class="ml-auto shrink-0 text-[10px] tabular-nums text-muted-foreground">{{ g.todos.length }}</span>
        </div>
        <div class="max-h-48 overflow-auto divide-y divide-border/50">
          <div v-for="q in g.todos" :key="q.todo.id" class="flex items-center gap-2 pl-3 pr-1 py-1">
            <Checkbox class="min-w-0 flex-1" :model-value="picked.has(q.todo.id)" @update:model-value="toggle(q.todo.id)">
              <span class="truncate text-xs">{{ q.todo.text.split('\n')[0] || t('board.untitled') }}</span>
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
    </div>

    <button
      type="button"
      class="flex items-center gap-1 text-[11px] font-medium text-muted-foreground hover:text-foreground cursor-pointer"
      :aria-expanded="showOptions"
      @click="showOptions = !showOptions"
    >
      <component :is="showOptions ? ChevronDown : ChevronRight" class="h-3.5 w-3.5" :stroke-width="2" />
      {{ t('board.start.options') }}
    </button>
    <div v-if="showOptions" class="grid gap-3 sm:grid-cols-2">
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

    <Banner v-if="error" tone="destructive" class="whitespace-pre-line">{{ error }}</Banner>

    <div class="flex justify-end">
      <Button size="sm" :disabled="!picked.size || starting" @click="start">
        <Play class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ starting ? t('board.start.starting') : t('board.start.submit', { n: picked.size, projects: pickedProjects }, picked.size) }}
      </Button>
    </div>
  </Card>
</template>
