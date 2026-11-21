<script setup lang="ts">
// Command palette of the main window. Two modes:
//  - 'all' (⌘K): open tabs, recent sessions, screens and actions;
//  - 'quick' (⌘P): open tabs, recent sessions and the files open as tabs.
// Opened from the title-bar search button and the View / Go menu items (whose
// accelerators are the shortcuts — see lib/appMenu). Matching and ranking live
// in lib/paletteSearch (accent-insensitive, so "lich" finds "Lịch").
import { computed, nextTick, ref, watch, type Component } from 'vue'
import { entryIcon, entryLabel as tabEntryLabel } from '@/lib/tabMeta'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import {
  Columns2,
  ExternalLink,
  ListTodo,
  MessageSquare,
  PanelRightClose,
  SquareSplitHorizontal,
  ArrowLeftRight,
  Plus,
  Search,
  Settings,
  StickyNote,
  X,
} from 'lucide-vue-next'
import { Modal } from '@/components/ui'
import { useWorkspaceTabsStore, tabEntryKey, type TabEntry } from '@/stores/workspaceTabs'
import { useProjectsStore } from '@/stores/projects'
import { runActivityAt, useRunsStore, type RunRecord } from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { NAV_ROUTES } from '@/lib/navigation'
import { SURFACE_META } from '@/lib/surfaceMeta'
import { IS_MAC, isMenuActionBound, triggerMenuAction } from '@/lib/appMenu'
import { rankItems } from '@/lib/paletteSearch'
import { focusSessionWindow } from '@/lib/sessionWindow'
import {
  closeEntry,
  currentEntry,
  popOutCurrent,
  selectEntry,
  canOpenOnRight,
  idlePair,
  openOnRight,
  swapSplit,
  toggleSplitCurrent,
} from '@/lib/tabActions'

export type PaletteMode = 'all' | 'quick'

const props = defineProps<{ mode: PaletteMode | null }>()
const emit = defineEmits<{ 'update:mode': [value: PaletteMode | null] }>()

const { t } = useI18n()
const router = useRouter()
const tabsStore = useWorkspaceTabsStore()
const projectsStore = useProjectsStore()
const runsStore = useRunsStore()
const live = useLiveRunsStore()

type GroupId = 'tabs' | 'sessions' | 'files' | 'screens' | 'actions'
type SessionState = 'running' | 'pending' | 'idle'

interface PaletteItem {
  key: string
  group: GroupId
  label: string
  /** Secondary text after the label (project, folder…); also searched. */
  detail?: string
  keywords?: string[]
  icon?: Component
  /** Live status dot, for sessions. */
  state?: SessionState
  /** Shortcut hint on the right (⌘1…). */
  shortcut?: string
  run: () => unknown
}

const GROUP_ORDER: Record<PaletteMode, GroupId[]> = {
  all: ['tabs', 'sessions', 'screens', 'actions'],
  quick: ['tabs', 'sessions', 'files'],
}
const MAX_SESSIONS = 30
// With nothing typed, ⌘K shows a few recent sessions so the screens and
// actions below stay within reach; typing searches all MAX_SESSIONS.
const IDLE_SESSIONS = 8
const MOD = IS_MAC ? '⌘' : 'Ctrl+'

const query = ref('')
const active = ref(0)
const inputRef = ref<HTMLInputElement | null>(null)
const listRef = ref<HTMLElement | null>(null)

function close() {
  emit('update:mode', null)
}

// --- Items -------------------------------------------------------------------

function projectName(projectId: string): string {
  return projectsStore.projects.find((p) => p.id === projectId)?.name ?? ''
}

function entryLabel(e: TabEntry): string {
  return tabEntryLabel(e, t, (id) => projectName(id) || t('misc.tabs.project'))
}

function tabItem(e: TabEntry, index: number, group: GroupId): PaletteItem {
  const count = tabsStore.entries.length
  const isFile = e.type === 'surface' && e.surface === 'file'
  return {
    key: `tab:${tabEntryKey(e)}`,
    group,
    label: entryLabel(e),
    detail: isFile ? e.params.path : undefined,
    icon: entryIcon(e),
    // Same numbering as ⌘1…⌘8 / ⌘9 = last (lib/tabActions.goToTabNumber).
    shortcut: index < 8 ? `${MOD}${index + 1}` : index === count - 1 ? `${MOD}9` : undefined,
    run: () => selectEntry(e),
  }
}

function runName(r: RunRecord): string {
  if (r.title?.trim()) return r.title.trim()
  if (r.run_type !== 'session' && r.ref_number != null) {
    return `${r.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${r.ref_number}`
  }
  return t('palette.untitledSession')
}

function sessionState(r: RunRecord): SessionState {
  const s = live.sessions.get(r.id)
  if (s?.permissionQueue.length) return 'pending'
  if (s ? s.status === 'running' : r.status === 'running') return 'running'
  return 'idle'
}

/** Open a run where it lives: its own pop-out if it has one, else this window. */
async function openRun(r: RunRecord) {
  if (await focusSessionWindow(r.id)) return
  live.markSeen(r.id)
  tabsStore.open(r.project_id, r.id)
  tabsStore.focusedPane = 'left'
  await router
    .push({ name: 'project-run-detail', params: { projectId: r.project_id, runId: r.id } })
    .catch(() => {})
}

const recentRuns = computed(() =>
  [...runsStore.runMeta.values()]
    .sort((a, b) => runActivityAt(b).localeCompare(runActivityAt(a)))
    .slice(0, MAX_SESSIONS),
)

function sessionItems(): PaletteItem[] {
  return recentRuns.value.map((r) => {
    const project = projectName(r.project_id)
    return {
      key: `run:${r.id}`,
      group: 'sessions',
      label: runName(r),
      detail: project,
      keywords: [project, r.worktree_branch ?? '', r.ref_title ?? ''].filter(Boolean),
      icon: MessageSquare,
      state: sessionState(r),
      run: () => openRun(r),
    }
  })
}

function screenItems(): PaletteItem[] {
  const routes: PaletteItem[] = NAV_ROUTES.map((r) => ({
    key: `go:${r.path}`,
    group: 'screens',
    label: t(r.labelKey),
    icon: r.icon,
    // Same actions as the sidebar / Go menu (bound in App.vue).
    run: () => triggerMenuAction(`go.${r.path}`),
  }))
  const lists: PaletteItem[] = (
    [
      ['todos', 'window.todoList'],
      ['notes', 'window.noteList'],
      ['slack', 'window.slackThreads'],
    ] as const
  ).map(([kind, action]) => ({
    key: `go:${kind}`,
    group: 'screens',
    label: t(SURFACE_META[kind].labelKey),
    icon: SURFACE_META[kind].icon,
    run: () => triggerMenuAction(action),
  }))
  return [...routes, ...lists]
}

function actionItems(): PaletteItem[] {
  const out: PaletteItem[] = []
  if (isMenuActionBound('file.newSession')) {
    out.push({
      key: 'action:newSession',
      group: 'actions',
      label: t('palette.actions.newSession'),
      icon: Plus,
      run: () => triggerMenuAction('file.newSession'),
    })
  }
  out.push(
    {
      key: 'action:newTodo',
      group: 'actions',
      label: t('palette.actions.newTodo'),
      icon: ListTodo,
      run: () => triggerMenuAction('file.newTodo'),
    },
    {
      key: 'action:newNote',
      group: 'actions',
      label: t('palette.actions.newNote'),
      icon: StickyNote,
      run: () => triggerMenuAction('file.newNote'),
    },
  )
  if (tabsStore.splitId) {
    out.push(
      {
        key: 'action:unsplit',
        group: 'actions',
        label: t('palette.actions.unsplit'),
        icon: PanelRightClose,
        shortcut: `${MOD}\\`,
        run: toggleSplitCurrent,
      },
      {
        key: 'action:swapSplit',
        group: 'actions',
        label: t('palette.actions.swapSplit'),
        icon: ArrowLeftRight,
        run: swapSplit,
      },
    )
  } else {
    const pair = idlePair()
    if (pair) {
      out.push({
        key: 'action:restoreSplit',
        group: 'actions',
        label: t('palette.actions.restoreSplit', { left: entryLabel(pair.left), right: entryLabel(pair.right) }),
        icon: Columns2,
        shortcut: `${MOD}\\`,
        run: toggleSplitCurrent,
      })
    }
  }
  // "Open … on the right" for every other open tab that may sit beside the left one.
  for (const e of tabsStore.entries) {
    if (!canOpenOnRight(e)) continue
    out.push({
      key: `action:openRight:${tabEntryKey(e)}`,
      group: 'actions',
      label: t('palette.actions.openOnRight', { name: entryLabel(e) }),
      icon: SquareSplitHorizontal,
      run: () => openOnRight(e),
    })
  }
  const cur = currentEntry()
  if (cur?.type === 'surface') {
    out.push({
      key: 'action:popOut',
      group: 'actions',
      label: t('palette.actions.popOut', { name: entryLabel(cur) }),
      icon: ExternalLink,
      run: popOutCurrent,
    })
  }
  if (cur) {
    out.push({
      key: 'action:closeTab',
      group: 'actions',
      label: t('palette.actions.closeTab', { name: entryLabel(cur) }),
      icon: X,
      shortcut: IS_MAC ? '⇧⌘W' : 'Ctrl+Shift+W',
      run: () => closeEntry(cur),
    })
  }
  out.push({
    key: 'action:windowSettings',
    group: 'actions',
    label: t('palette.actions.windowSettings'),
    icon: Settings,
    run: () => router.push({ path: '/settings', query: { section: 'general' } }).catch(() => {}),
  })
  return out
}

const items = computed<PaletteItem[]>(() => {
  const mode = props.mode
  if (!mode) return []
  const tabs: PaletteItem[] = []
  const files: PaletteItem[] = []
  tabsStore.entries.forEach((e, i) => {
    // ⌘P lists the files on their own; ⌘K keeps them with the other tabs.
    if (mode === 'quick' && e.type === 'surface' && e.surface === 'file') files.push(tabItem(e, i, 'files'))
    else tabs.push(tabItem(e, i, 'tabs'))
  })
  let sessions = sessionItems()
  if (mode === 'all' && !query.value.trim()) sessions = sessions.slice(0, IDLE_SESSIONS)
  return mode === 'quick'
    ? [...tabs, ...sessions, ...files]
    : [...tabs, ...sessions, ...screenItems(), ...actionItems()]
})

const results = computed(() =>
  props.mode
    ? rankItems(
        items.value.map((i) => ({ ...i, keywords: [...(i.keywords ?? []), ...(i.detail ? [i.detail] : [])] })),
        query.value,
        GROUP_ORDER[props.mode],
      )
    : [],
)

/** Index of the first result of each group, for the group headings. */
const groupStarts = computed(() => {
  const starts = new Set<number>()
  results.value.forEach((r, i) => {
    if (i === 0 || results.value[i - 1].group !== r.group) starts.add(i)
  })
  return starts
})

// --- Loading sessions ----------------------------------------------------------
// `runMeta` only holds the projects this window has opened; fill it for every
// project (one local `list_runs` each) when the palette opens, at most every 30s.
const loadingSessions = ref(false)
let loadedAt = 0

async function loadSessions() {
  if (loadingSessions.value || Date.now() - loadedAt < 30_000) return
  loadingSessions.value = true
  try {
    if (projectsStore.projects.length === 0) await projectsStore.fetchProjects().catch(() => {})
    await Promise.allSettled(projectsStore.projects.map((p) => runsStore.refreshMeta(p.id)))
    loadedAt = Date.now()
  } finally {
    loadingSessions.value = false
  }
}

// --- Open / keyboard -------------------------------------------------------------

watch(
  () => props.mode,
  async (mode, prev) => {
    if (!mode) return
    if (mode !== prev) {
      query.value = ''
      active.value = 0
    }
    void loadSessions()
    await nextTick()
    inputRef.value?.focus()
    inputRef.value?.select()
  },
)

watch(query, () => {
  active.value = 0
})

watch(results, (list) => {
  if (active.value >= list.length) active.value = Math.max(0, list.length - 1)
})

watch(active, async (i) => {
  await nextTick()
  listRef.value?.querySelector<HTMLElement>(`[data-index="${i}"]`)?.scrollIntoView({ block: 'nearest' })
})

function pick(item: PaletteItem | undefined) {
  if (!item) return
  close()
  void Promise.resolve(item.run()).catch(() => {})
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing) return
  const n = results.value.length
  if (e.key === 'ArrowDown') {
    e.preventDefault()
    if (n) active.value = (active.value + 1) % n
  } else if (e.key === 'ArrowUp') {
    e.preventDefault()
    if (n) active.value = (active.value - 1 + n) % n
  } else if (e.key === 'Enter') {
    e.preventDefault()
    pick(results.value[active.value])
  } else if (e.key === 'Escape') {
    // Handled here rather than by Modal's window listener so the key doesn't
    // also reach the screen underneath.
    e.preventDefault()
    e.stopPropagation()
    close()
  }
}

const STATE_DOT: Record<SessionState, string> = {
  pending: 'bg-warning',
  running: 'bg-primary',
  idle: 'bg-muted-foreground/40',
}
</script>

<template>
  <Modal :open="mode !== null" size="lg" hide-header @close="close">
    <div class="flex flex-col">
      <div class="flex items-center gap-2 border-b border-border px-3 h-11">
        <Search class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <input
          ref="inputRef"
          v-model="query"
          type="text"
          class="flex-1 min-w-0 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
          :placeholder="mode === 'quick' ? t('palette.quickPlaceholder') : t('palette.placeholder')"
          :aria-label="t('palette.open')"
          role="combobox"
          aria-autocomplete="list"
          aria-controls="command-palette-list"
          :aria-expanded="true"
          :aria-activedescendant="results.length ? `command-palette-item-${active}` : undefined"
          spellcheck="false"
          autocomplete="off"
          @keydown="onKeydown"
        />
        <span v-if="loadingSessions" class="text-[11px] text-muted-foreground shrink-0">
          {{ t('palette.loadingSessions') }}
        </span>
      </div>

      <div
        id="command-palette-list"
        ref="listRef"
        role="listbox"
        class="h-[min(60vh,420px)] overflow-y-auto py-1"
      >
        <p v-if="results.length === 0" class="px-3 py-6 text-center text-xs text-muted-foreground">
          {{ t('palette.empty') }}
        </p>
        <template v-for="(item, index) in results" :key="item.key">
          <div
            v-if="groupStarts.has(index)"
            class="px-3 pt-2.5 pb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
          >
            {{ t(`palette.groups.${item.group}`) }}
          </div>
          <button
            :id="`command-palette-item-${index}`"
            type="button"
            role="option"
            :aria-selected="index === active"
            :data-index="index"
            class="flex w-full items-center gap-2.5 h-8 px-3 text-left cursor-pointer"
            :class="index === active ? 'bg-primary/15 text-foreground' : 'text-foreground/90'"
            @mousedown.prevent
            @mousemove="active = index"
            @click="pick(item)"
          >
            <span v-if="item.state" class="relative flex h-2 w-2 shrink-0">
              <span
                v-if="item.state === 'running'"
                class="absolute inline-flex h-full w-full rounded-full bg-primary opacity-60 animate-ping"
              />
              <span class="relative inline-flex h-2 w-2 rounded-full" :class="STATE_DOT[item.state]" />
            </span>
            <component
              :is="item.icon"
              v-if="item.icon"
              class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
              :stroke-width="1.75"
            />
            <span class="truncate text-[13px]">{{ item.label }}</span>
            <span v-if="item.detail" class="truncate text-[11px] text-muted-foreground min-w-0">{{ item.detail }}</span>
            <span v-if="item.shortcut" class="ml-auto shrink-0 pl-2 text-[10px] text-muted-foreground">
              {{ item.shortcut }}
            </span>
          </button>
        </template>
      </div>

      <div class="flex items-center gap-3 border-t border-border px-3 py-1.5 text-[10px] text-muted-foreground">
        <span>↑↓ {{ t('palette.hint.navigate') }}</span>
        <span>↵ {{ t('palette.hint.open') }}</span>
        <span>Esc {{ t('palette.hint.close') }}</span>
      </div>
    </div>
  </Modal>
</template>
