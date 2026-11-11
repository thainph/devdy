<script setup lang="ts">
// Left pane of the Secret notes window — the sibling of ItemSidebar, same
// layout: search + the window's pin / lock, project filter + "+ New" (as wide
// as pin + lock, so the select lines up with the search box), the note count
// and a ⇅ sort menu, then the notes grouped by project (each header
// collapsible, remembered in localStorage). Sorting by last edit or title
// instead gives one flat list, each row naming its project; last edit is the
// default, the sort is remembered in localStorage.
// The footer holds the vault itself: the auto-lock note and Change password.
//
// A row is the note's title and its age: there is no preview, the content is
// only fetched on Reveal.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertCircle,
  ArrowDownWideNarrow,
  Check,
  ChevronDown,
  ChevronRight,
  Folder,
  Loader2,
  Lock,
  Pin,
  PinOff,
  Plus,
  Search,
  X,
} from 'lucide-vue-next'
import { AppSelect, Banner, Button, DropdownItem, DropdownMenu, Input } from '@/components/ui'
import { groupClass, groupHeaderClass, rowClass, rowListClass, rowSubClass, rowTitleClass } from '@/components/listDetail/rowStyles'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate } from '@/lib/slackThreadUtils'
import type { SecretNoteMeta } from '@/lib/secretVault'

const props = defineProps<{
  /** Already filtered. */
  notes: SecretNoteMeta[]
  /** True when the vault holds no note at all (vs. none matching the filters). */
  emptyVault: boolean
  selectedId: string | null
  projectOptions: { value: string; label: string }[]
  /** project id → name, for the group headers. */
  projectNames: Record<string, string>
  pinned: boolean
  /** Notes of an older vault, mergeable with its password. */
  legacyCount: number
  /** Notes whose vault has not reached this device yet. */
  pendingCount: number
  merging: boolean
  autoLockMinutes: number
}>()

const search = defineModel<string>('search', { required: true })
const projectFilter = defineModel<string>('project', { required: true })
const mergePassword = defineModel<string>('mergePassword', { required: true })

const emit = defineEmits<{
  select: [id: string]
  add: []
  togglePin: []
  lock: []
  merge: []
  changePassword: []
}>()

const { t } = useI18n()

const COLLAPSED_KEY = 'items.secret.collapsed'

function loadCollapsed(): Set<string> {
  try {
    const raw = JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? '[]')
    return new Set(Array.isArray(raw) ? raw.filter((x) => typeof x === 'string') : [])
  } catch {
    return new Set()
  }
}

const collapsed = ref<Set<string>>(loadCollapsed())

function toggle(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]))
  } catch {
    /* storage unavailable */
  }
}

const NONE = '__none__'

// ── Grouped by project, or one flat list by another sort ───────────────────
type SortMode = 'project' | 'updated' | 'title'
const SORT_KEY = 'items.secret.order'
const SORTS = computed<{ value: SortMode; label: string }[]>(() => [
  { value: 'project', label: t('item.sortByProject') },
  { value: 'updated', label: t('item.sortByUpdated') },
  { value: 'title', label: t('item.sortByTitle') },
])
const saved = localStorage.getItem(SORT_KEY)
/** Flat by default: the last edited first. */
const DEFAULT_SORT: SortMode = 'updated'
const sortMode = ref<SortMode>(SORTS.value.find((o) => o.value === saved)?.value ?? DEFAULT_SORT)
watch(sortMode, (m) => {
  try {
    localStorage.setItem(SORT_KEY, m)
  } catch {
    /* storage unavailable */
  }
})
const flat = computed(() => sortMode.value !== 'project')

const flatNotes = computed(() =>
  [...props.notes].sort((a, b) => {
    if (sortMode.value === 'updated') return a.updated_at < b.updated_at ? 1 : a.updated_at > b.updated_at ? -1 : 0
    // By title: untitled last.
    return (!a.title ? 1 : 0) - (!b.title ? 1 : 0) || a.title.localeCompare(b.title)
  }),
)

/**
 * Notes by project (A–Z), "No project" last; vault order kept inside a group.
 * Any other sort: a single header-less group.
 */
const groups = computed(() => {
  if (flat.value) return [{ key: '__flat__', label: '', notes: flatNotes.value }]
  const map = new Map<string, SecretNoteMeta[]>()
  for (const n of props.notes) {
    const key = n.project_id ?? NONE
    const list = map.get(key)
    if (list) list.push(n)
    else map.set(key, [n])
  }
  return [...map.entries()]
    .map(([key, notes]) => ({
      key,
      label: key === NONE ? t('item.noProject') : props.projectNames[key] ?? key,
      notes,
    }))
    .sort((a, b) => (a.key === NONE ? 1 : b.key === NONE ? -1 : a.label.localeCompare(b.label)))
})
</script>

<template>
  <aside class="list-sidebar flex w-[360px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <!-- Search + pin / lock, filter + new -->
    <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
      <div class="flex items-center gap-1.5">
        <div class="relative min-w-0 flex-1">
          <Search
            class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <Input v-model="search" class="pl-8 pr-7" :placeholder="t('secretNotes.search')" />
          <button
            v-if="search"
            type="button"
            class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
            :title="t('item.clearSearch')"
            @click="search = ''"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>
        <Button
          :variant="pinned ? 'primary' : 'outline'"
          size="icon"
          class="shrink-0"
          :aria-pressed="pinned"
          :title="pinned ? t('item.unpin') : t('item.pin')"
          :aria-label="pinned ? t('item.unpin') : t('item.pin')"
          @click="emit('togglePin')"
        >
          <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          class="shrink-0"
          :title="t('secretNotes.lock')"
          :aria-label="t('secretNotes.lock')"
          @click="emit('lock')"
        >
          <Lock class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </div>
      <div class="flex items-center gap-1.5">
        <div class="min-w-0 flex-1">
          <AppSelect v-model="projectFilter" :options="projectOptions" size="sm">
            <template #leading>
              <Folder class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            </template>
          </AppSelect>
        </div>
        <!-- As wide as pin + lock above: the select lines up with the search box. -->
        <Button size="sm" class="w-[70px] shrink-0 px-0" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('secretNotes.new') }}
        </Button>
      </div>
      <div class="flex items-center gap-1.5">
        <span class="min-w-0 flex-1 truncate px-1 text-[11px] text-muted-foreground">
          {{ t('secretNotes.count', { n: notes.length }, notes.length) }}
        </span>
        <!-- Sort; tinted when not on the default. -->
        <DropdownMenu align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-xs"
              class="shrink-0"
              :class="sortMode !== DEFAULT_SORT && 'border-primary/50 text-primary'"
              :title="t('item.sortTitle')"
              :aria-label="t('item.sortTitle')"
            >
              <ArrowDownWideNarrow class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <div class="px-2.5 pb-1 pt-1.5 text-[11px] font-medium text-muted-foreground">
            {{ t('item.sortTitle') }}
          </div>
          <DropdownItem v-for="o in SORTS" :key="o.value" @click="sortMode = o.value">
            <Check class="h-3.5 w-3.5" :class="sortMode === o.value ? '' : 'invisible'" :stroke-width="2" />
            {{ o.label }}
          </DropdownItem>
        </DropdownMenu>
      </div>
    </div>

    <!-- Groups -->
    <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <!-- Notes of another vault (backup import): merge with its password. -->
      <form v-if="legacyCount" class="mb-2" @submit.prevent="emit('merge')">
        <Banner tone="warning" :icon="AlertCircle">
          <p>{{ t('secretNotes.merge.help', { n: legacyCount }) }}</p>
          <div class="mt-1.5 flex items-center gap-1.5">
            <Input
              v-model="mergePassword"
              type="password"
              autocomplete="off"
              size="xs"
              :placeholder="t('secretNotes.merge.password')"
            />
            <Button type="submit" size="xs" class="shrink-0" :disabled="merging || !mergePassword">
              <Loader2 v-if="merging" class="h-3 w-3 animate-spin" :stroke-width="1.75" />
              {{ t('secretNotes.merge.button') }}
            </Button>
          </div>
        </Banner>
      </form>
      <p v-if="pendingCount" class="mb-2 px-1.5 text-[11px] text-muted-foreground">
        {{ t('secretNotes.pending', { n: pendingCount }) }}
      </p>

      <div v-if="emptyVault" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">{{ t('secretNotes.empty') }}</p>
        <p class="mt-1 text-[11px] text-muted-foreground/70">{{ t('secretNotes.emptyHint') }}</p>
      </div>
      <p v-else-if="notes.length === 0" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        {{ t('item.noResults') }}
      </p>

      <div v-for="g in groups" v-else :key="g.key" :class="groupClass">
        <div v-if="!flat" :class="groupHeaderClass">
        <button
          type="button"
          class="flex min-w-0 flex-1 cursor-pointer items-center gap-1 px-1.5 py-1 text-left text-xs font-semibold text-foreground"
          :aria-expanded="!collapsed.has(g.key)"
          @click="toggle(g.key)"
        >
          <component
            :is="collapsed.has(g.key) ? ChevronRight : ChevronDown"
            class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
            :stroke-width="2"
          />
          <Folder class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="min-w-0 flex-1 truncate">{{ g.label }}</span>
          <span class="shrink-0 text-[10px] font-normal tabular-nums text-muted-foreground/80">
            {{ g.notes.length }}
          </span>
        </button>
        </div>

        <ul v-if="flat || !collapsed.has(g.key)" :class="rowListClass">
          <li v-for="n in g.notes" :key="n.id">
            <button
              type="button"
              class="cursor-pointer flex-col pr-2 text-left"
              :class="[flat ? 'pl-2' : 'pl-6', rowClass(n.id === selectedId)]"
              :title="n.title || t('secretNotes.untitled')"
              @click="emit('select', n.id)"
            >
              <span class="flex w-full min-w-0 items-center gap-1.5">
                <span class="min-w-0 flex-1 truncate" :class="rowTitleClass">{{ n.title || t('secretNotes.untitled') }}</span>
                <span class="shrink-0 text-[10px] tabular-nums text-muted-foreground/80" :title="formatFullDate(n.updated_at)">
                  {{ relativeAge(n.updated_at) }}
                </span>
              </span>
              <!-- Flat list: the note's project. -->
              <span v-if="flat" class="flex w-full min-w-0 items-center gap-1" :class="rowSubClass">
                <Folder class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                <span class="truncate">
                  {{ n.project_id ? projectNames[n.project_id] ?? n.project_id : t('item.noProject') }}
                </span>
              </span>
            </button>
          </li>
        </ul>
      </div>
    </div>

    <!-- The vault: auto-lock + change password. -->
    <div class="flex shrink-0 items-center gap-2 border-t border-border/60 px-3 py-2 text-[10px] text-muted-foreground">
      <span class="min-w-0 flex-1">{{ t('secretNotes.autoLock', { n: autoLockMinutes }) }}</span>
      <button
        type="button"
        class="shrink-0 cursor-pointer underline-offset-2 hover:text-foreground hover:underline"
        @click="emit('changePassword')"
      >
        {{ t('secretNotes.password.title') }}
      </button>
    </div>
  </aside>
</template>
