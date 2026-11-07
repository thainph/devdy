<script setup lang="ts">
// Focus: which of my open todos are late, per project (lib/focusRadar). The
// board comes from the backend (`focus_board`, kept live by the focus store);
// summary cards filter it by status, each project row folds open its todos
// (most late first). A todo is ticked done in place; a click opens the detail
// drawer. A reminder click (`focus:open-todo`, or the pending id taken on mount
// when the click created this window) opens that todo's drawer and flashes it.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Check, ChevronRight, Play, SearchX, Target } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { Banner, Button, Card, Input, PageHeader, Skeleton } from '@/components/ui'
import AddTodoButton from '@/components/focus/AddTodoButton.vue'
import FocusDialogs from '@/components/focus/FocusDialogs.vue'
import FocusAiDialogs from '@/components/focus/FocusAiDialogs.vue'
import FocusTodoDrawer from '@/components/focus/FocusTodoDrawer.vue'
import { useFocusActions } from '@/components/focus/useFocusActions'
import { useFocusStore } from '@/stores/focus'
import { useProjectsStore } from '@/stores/projects'
import { FOCUS_OPEN_TODO, projectHue, type FocusOpenTodo } from '@/lib/focus'
import { firstLine } from '@/lib/itemText'
import {
  classifyTodos,
  countFocus,
  filterFocus,
  groupByProject,
  DUE_SOON_DAYS,
  type FocusProjectGroup,
  type FocusRadarItem,
  type FocusRadarStatus,
} from '@/lib/focusRadar'

const { t } = useI18n()
const store = useFocusStore()
const projects = useProjectsStore()
const actions = useFocusActions()

// ── Classification ──
const now = computed(() => store.now)
const classified = computed(() => classifyTodos(store.items, now.value))
const counts = computed(() => countFocus(classified.value))

const statusFilter = ref<FocusRadarStatus | null>(null)
const query = ref('')
const groups = computed(() =>
  groupByProject(filterFocus(classified.value, { status: statusFilter.value, query: query.value })),
)
function toggleStatus(s: FocusRadarStatus) {
  statusFilter.value = statusFilter.value === s ? null : s
}

// Cards / columns ("later" only counts towards "Open").
const COLUMNS: FocusRadarStatus[] = ['overdue', 'today', 'due-soon', 'waiting', 'no-due']

const TONE_TEXT: Record<FocusRadarStatus, string> = {
  overdue: 'text-destructive',
  today: 'text-warning',
  'due-soon': 'text-info',
  waiting: 'text-foreground',
  'no-due': 'text-muted-foreground',
  later: 'text-success',
}
const TONE_PILL: Record<FocusRadarStatus, string> = {
  overdue: 'bg-destructive/15 text-destructive border-destructive/30',
  today: 'bg-warning/15 text-warning border-warning/30',
  'due-soon': 'bg-info/15 text-info border-info/30',
  waiting: 'bg-muted text-foreground border-border',
  'no-due': 'bg-muted/60 text-muted-foreground border-border/70',
  later: 'bg-success/15 text-success border-success/30',
}

function statusLabel(s: FocusRadarStatus): string {
  return t(`focus.radar.status.${s}`, { days: DUE_SOON_DAYS })
}

// ── Project rows (open by default; folded ones remembered) ──
const COLLAPSED_KEY = 'devdy:focusCollapsedProjects'
const NO_PROJECT = '__none'
const collapsed = ref<Set<string>>(new Set(readCollapsed()))
function readCollapsed(): string[] {
  try {
    return JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? '[]') as string[]
  } catch {
    return []
  }
}
function groupKey(g: FocusProjectGroup): string {
  return g.projectId ?? NO_PROJECT
}
function setCollapsed(next: Set<string>) {
  collapsed.value = next
  localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]))
}
function toggleGroup(g: FocusProjectGroup) {
  const next = new Set(collapsed.value)
  if (next.has(groupKey(g))) next.delete(groupKey(g))
  else next.add(groupKey(g))
  setCollapsed(next)
}
const allOpen = computed(() => groups.value.every((g) => !collapsed.value.has(groupKey(g))))
function toggleAll() {
  setCollapsed(allOpen.value ? new Set(groups.value.map(groupKey)) : new Set())
}
// Narrowing to one status or a search means "show me those todos".
function isOpen(g: FocusProjectGroup): boolean {
  return !!statusFilter.value || !!query.value.trim() || !collapsed.value.has(groupKey(g))
}

function projectName(id: string | null): string {
  if (!id) return t('focus.radar.noProject')
  return projects.projects.find((p) => p.id === id)?.name ?? id
}
function dotStyle(id: string | null) {
  return id ? { backgroundColor: `hsl(${projectHue(id)} 70% 50%)` } : {}
}

// ── Todo rows ──
function title(i: FocusRadarItem): string {
  return firstLine(i.item.todo.text) || i.item.todo.text
}
function deadlineText(i: FocusRadarItem): string {
  const { daysLeft } = i
  const time = i.item.todo.due_time
  if (daysLeft === null) return t('focus.radar.noDeadline')
  if (daysLeft < 0) return t('focus.radar.lateDays', { n: -daysLeft })
  if (daysLeft === 0) {
    if (i.radar === 'overdue') return t('focus.radar.lateToday', { time: time ?? '' })
    return time ? t('focus.radar.dueTodayAt', { time }) : t('focus.radar.dueToday')
  }
  return t('focus.radar.daysLeft', { n: daysLeft })
}
function deadlineDate(i: FocusRadarItem): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(i.item.todo.due_date ?? '')
  return m && i.daysLeft !== 0 ? `${m[3]}/${m[2]}` : ''
}

// ── Drawer ──
const openId = ref<string | null>(null)
const openItem = computed(() => (openId.value ? store.items.find((it) => it.todo.id === openId.value) ?? null : null))
// Deleted elsewhere → close.
watch(openItem, (it) => {
  if (!it && openId.value && store.loaded) openId.value = null
})

// ── Reminder clicks → open + flash the todo ──
const flashId = ref<string | null>(null)
let flashTimer: ReturnType<typeof setTimeout> | null = null
async function showTodo(id: string | null | undefined) {
  if (!id) return
  if (!store.loaded) await store.load()
  const it = store.items.find((x) => x.todo.id === id)
  if (!it) return
  openId.value = id
  statusFilter.value = null
  query.value = ''
  const key = it.todo.project_id ?? NO_PROJECT
  if (collapsed.value.has(key)) {
    const next = new Set(collapsed.value)
    next.delete(key)
    setCollapsed(next)
  }
  flashId.value = id
  if (flashTimer) clearTimeout(flashTimer)
  flashTimer = setTimeout(() => (flashId.value = null), 2500)
  await nextTick()
  document.querySelector(`[data-focus-row="${CSS.escape(id)}"]`)?.scrollIntoView({ block: 'center', behavior: 'smooth' })
}

let unlistenOpen: UnlistenFn | null = null
onMounted(async () => {
  void store.start()
  if (!projects.projects.length) void projects.fetchProjects()
  try {
    unlistenOpen = await listen<FocusOpenTodo>(FOCUS_OPEN_TODO, (e) => {
      // Consume the pending id too, so a later window mount doesn't reopen it.
      void invoke('focus_take_open_todo').catch(() => {})
      void showTodo(e.payload?.todo_id)
    })
    void showTodo(await invoke<string | null>('focus_take_open_todo'))
  } catch {
    /* outside the Tauri shell */
  }
})
onBeforeUnmount(() => {
  store.stop()
  unlistenOpen?.()
  if (flashTimer) clearTimeout(flashTimer)
})

const GRID = 'grid grid-cols-[minmax(160px,1.4fr)_repeat(5,minmax(64px,84px))_52px_76px] items-center gap-x-2'
</script>

<template>
  <div class="flex h-full flex-col">
    <PageHeader :title="t('focus.title')" :count="store.loaded && classified.length > 0 ? classified.length : null">
      <template #actions>
        <Input v-model="query" size="sm" class="w-56" :placeholder="t('focus.radar.searchPlaceholder')" />
        <AddTodoButton button-variant="primary" size="sm" />
      </template>
    </PageHeader>

    <!-- First load -->
    <div v-if="!store.loaded && store.loading" class="space-y-4 p-6">
      <div class="grid grid-cols-5 gap-3">
        <div v-for="i in 5" :key="i" class="space-y-2.5 rounded-lg border border-border/60 bg-card p-3">
          <Skeleton class="h-2.5 w-16 rounded" />
          <Skeleton class="h-6 w-10 rounded" />
        </div>
      </div>
      <Skeleton v-for="i in 5" :key="i" class="h-10 w-full rounded" />
    </div>

    <!-- Error before anything loaded -->
    <div v-else-if="store.error && !store.loaded" class="p-6">
      <Banner tone="destructive" class="items-center px-4 py-3 text-sm">
        {{ t('focus.loadError') }}: {{ store.error }}
        <template #actions>
          <Button variant="outline" size="sm" @click="store.load()">{{ t('focus.retry') }}</Button>
        </template>
      </Banner>
    </div>

    <!-- Nothing open -->
    <div v-else-if="classified.length === 0" class="flex min-h-80 flex-1 flex-col items-center justify-center text-center">
      <div class="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
        <Target class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-sm font-medium">{{ t('focus.empty.title') }}</p>
      <p class="mt-1 max-w-96 text-xs text-muted-foreground">{{ t('focus.empty.hint') }}</p>
    </div>

    <div v-else class="min-h-0 flex-1 overflow-y-auto">
      <div class="space-y-4 p-6">
        <!-- Summary cards (click = filter to that status) -->
        <div class="grid grid-cols-5 gap-3">
          <Card
            v-for="s in COLUMNS"
            :key="s"
            body-class="p-3"
            role="button"
            tabindex="0"
            class="cursor-pointer transition-colors hover:bg-accent/50"
            :class="statusFilter === s ? 'ring-2 ring-primary/60' : ''"
            :title="t(`focus.radar.hint.${s}`, { days: DUE_SOON_DAYS })"
            :aria-pressed="statusFilter === s"
            @click="toggleStatus(s)"
            @keydown.enter.prevent="toggleStatus(s)"
          >
            <div class="text-[11px] text-muted-foreground">{{ statusLabel(s) }}</div>
            <div class="mt-0.5 text-xl font-semibold" :class="counts[s] > 0 ? TONE_TEXT[s] : ''">{{ counts[s] }}</div>
          </Card>
        </div>

        <!-- Empty after filtering -->
        <div v-if="groups.length === 0" class="flex min-h-60 flex-col items-center justify-center text-center">
          <div class="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
            <SearchX class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <p class="text-sm font-medium">{{ t('focus.empty.filtered') }}</p>
        </div>

        <!-- Per-project rollup -->
        <Card v-else body-class="text-xs">
          <div :class="GRID" class="sticky top-0 z-10 h-8 border-b border-border/60 bg-muted/30 px-3 text-[11px] font-medium text-muted-foreground">
            <button class="flex items-center gap-1 text-left hover:text-foreground" @click="toggleAll">
              <ChevronRight class="h-3 w-3 transition-transform" :class="{ 'rotate-90': allOpen }" :stroke-width="2" />
              {{ t('focus.radar.colProject') }}
            </button>
            <div v-for="s in COLUMNS" :key="s" class="truncate text-center">{{ statusLabel(s) }}</div>
            <div class="text-center">{{ t('focus.radar.colOpen') }}</div>
            <div class="text-center">{{ t('focus.radar.colWorst') }}</div>
          </div>

          <div v-for="g in groups" :key="groupKey(g)" class="border-b border-border/40 last:border-b-0">
            <button
              :class="GRID"
              class="h-10 w-full px-3 text-left transition-colors hover:bg-accent/50"
              :aria-expanded="isOpen(g)"
              @click="toggleGroup(g)"
            >
              <span class="flex min-w-0 items-center gap-1.5">
                <ChevronRight class="h-3 w-3 shrink-0 text-muted-foreground transition-transform" :class="{ 'rotate-90': isOpen(g) }" :stroke-width="2" />
                <span v-if="g.projectId" class="h-2 w-2 shrink-0 rounded-full" :style="dotStyle(g.projectId)" />
                <span class="truncate text-[13px] font-medium" :class="{ 'italic text-muted-foreground': !g.projectId }">{{ projectName(g.projectId) }}</span>
              </span>
              <span v-for="s in COLUMNS" :key="s" class="flex justify-center">
                <span
                  v-if="g.counts[s] > 0"
                  class="min-w-7 rounded-full border px-1.5 py-0.5 text-center text-[11px] font-semibold"
                  :class="TONE_PILL[s]"
                >{{ g.counts[s] }}</span>
                <span v-else class="text-muted-foreground/40">–</span>
              </span>
              <span class="text-center font-medium">{{ g.total }}</span>
              <span class="text-center" :class="g.worstLateDays !== null ? 'font-medium text-destructive' : 'text-muted-foreground/40'">
                {{ g.worstLateDays === null ? '–' : g.worstLateDays === 0 ? t('focus.radar.today') : t('focus.radar.lateDays', { n: g.worstLateDays }) }}
              </span>
            </button>

            <div v-if="isOpen(g)" class="border-t border-border/30 bg-muted/20 py-1">
              <div
                v-for="i in g.items"
                :key="i.item.todo.id"
                :data-focus-row="i.item.todo.id"
                role="button"
                tabindex="0"
                class="group flex h-8 cursor-pointer items-center gap-2 pl-7 pr-3 hover:bg-accent/50"
                :class="flashId === i.item.todo.id ? 'ring-2 ring-inset ring-primary/70' : ''"
                @click="openId = i.item.todo.id"
                @keydown.enter.self="openId = i.item.todo.id"
              >
                <button
                  type="button"
                  class="flex h-4 w-4 shrink-0 items-center justify-center rounded border border-muted-foreground/40 transition-colors hover:border-primary hover:bg-primary/10"
                  :title="t('focus.actions.done')"
                  :aria-label="t('focus.actions.done')"
                  @click.stop="actions.done(i.item.todo)"
                >
                  <Check class="h-3 w-3 opacity-0 group-hover:opacity-40" :stroke-width="2.5" />
                </button>
                <span class="w-24 shrink-0 truncate rounded border px-1.5 py-0.5 text-center text-[10px] font-medium" :class="TONE_PILL[i.radar]">
                  {{ t(`focus.radar.status.${i.radar}`, { days: DUE_SOON_DAYS }) }}
                </span>
                <span class="min-w-0 flex-1 truncate text-[13px]" :title="i.item.todo.text">{{ title(i) }}</span>
                <span v-if="i.item.todo.status === 'doing'" class="flex shrink-0 items-center gap-0.5 text-[11px] text-primary">
                  <Play class="h-3 w-3" :stroke-width="2" />{{ t('focus.status.doing') }}
                </span>
                <span v-if="i.item.todo.priority === 1" class="shrink-0 rounded bg-destructive px-1 py-px text-[9px] font-bold text-destructive-foreground">P1</span>
                <span
                  class="w-32 shrink-0 text-right text-[11px]"
                  :class="i.radar === 'overdue' ? 'font-medium text-destructive' : i.radar === 'today' ? 'text-warning' : 'text-muted-foreground'"
                >
                  {{ deadlineText(i) }}<span v-if="deadlineDate(i)" class="text-muted-foreground/60"> · {{ deadlineDate(i) }}</span>
                </span>
              </div>
            </div>
          </div>
        </Card>
      </div>
    </div>

    <FocusTodoDrawer :item="openItem" @close="openId = null" />
    <FocusDialogs />
    <FocusAiDialogs />
  </div>
</template>
