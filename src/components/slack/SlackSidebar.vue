<script setup lang="ts">
// Left pane of the Slack threads window, laid out like Slack's sidebar: search,
// project filter + "Add thread", then a workspace → #channel tree (each header
// collapsible, remembered in localStorage) of thread rows. A row shows the
// title, the last reply's age (full date in the tooltip), a new-message badge,
// a status mark (spinner / error / warning) and an "Old" badge for legacy rows.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, ChevronDown, ChevronRight, Folder, Hash, Plus, RotateCw, Search, X } from 'lucide-vue-next'
import { AppSelect, Button, Input } from '@/components/ui'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate, groupThreads } from '@/lib/slackThreadUtils'
import type { SlackThreadProgress, SlackThreadSummary } from '@/stores/slackThreads'
import SlackStatusIcon from './SlackStatusIcon.vue'

const props = defineProps<{
  /** Already filtered by project / search. */
  threads: SlackThreadSummary[]
  /** True when there are no threads at all (vs. none matching the filters). */
  emptyStore: boolean
  selectedId: string | null
  progress: Record<string, SlackThreadProgress>
  projectOptions: { value: string; label: string }[]
  /** The list failed to load (shown above the tree with a retry). */
  error: string | null
}>()

const search = defineModel<string>('search', { required: true })
const projectFilter = defineModel<string>('project', { required: true })

const emit = defineEmits<{ select: [id: string]; add: []; retry: [] }>()

const { t } = useI18n()

const COLLAPSED_KEY = 'slackThreads.collapsed'

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

const tree = computed(() => groupThreads(props.threads))

function rowAge(th: SlackThreadSummary): string {
  return relativeAge(th.lastReplyAt ?? th.updatedAt)
}

function rowAgeTitle(th: SlackThreadSummary): string {
  return formatFullDate(th.lastReplyAt ?? th.updatedAt)
}
</script>

<template>
  <aside class="flex w-[260px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
    <!-- Search + filter + add -->
    <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
      <div class="relative">
        <Search
          class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
          :stroke-width="1.75"
        />
        <Input v-model="search" class="pl-8 pr-7" :placeholder="t('slackThread.sidebar.search')" />
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
      <div class="flex items-center gap-1.5">
        <div class="min-w-0 flex-1">
          <AppSelect v-model="projectFilter" :options="projectOptions" size="sm">
            <template #leading>
              <Folder class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            </template>
          </AppSelect>
        </div>
        <Button size="sm" class="shrink-0" :title="t('slackThread.sidebar.add')" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('slackThread.sidebar.add') }}
        </Button>
      </div>
    </div>

    <!-- Tree -->
    <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <div
        v-if="error"
        class="mb-2 flex items-start gap-1.5 rounded-md border border-destructive/30 bg-destructive/10 px-2 py-1.5 text-[11px] text-foreground"
      >
        <AlertCircle class="mt-px h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="2" />
        <span class="min-w-0 flex-1 break-words">{{ error }}</span>
        <button
          type="button"
          class="shrink-0 cursor-pointer rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          :title="t('slackThread.banner.retry')"
          :aria-label="t('slackThread.banner.retry')"
          @click="emit('retry')"
        >
          <RotateCw class="h-3 w-3" :stroke-width="2" />
        </button>
      </div>
      <div v-if="emptyStore && !error" class="px-3 py-8 text-center">
        <p class="text-xs text-muted-foreground">{{ t('slackThread.sidebar.empty') }}</p>
        <p class="mt-1 text-[11px] text-muted-foreground/70">{{ t('slackThread.sidebar.emptyHint') }}</p>
      </div>
      <p v-else-if="threads.length === 0" class="px-3 py-8 text-center text-[11px] text-muted-foreground">
        {{ t('slackThread.sidebar.noResults') }}
      </p>

      <div v-for="ws in tree" v-else :key="ws.key" class="mb-2">
        <button
          type="button"
          class="flex w-full cursor-pointer items-center gap-1 rounded px-1.5 py-1 text-left text-[13px] font-semibold text-foreground hover:bg-accent/50"
          :aria-expanded="!collapsed.has(ws.key)"
          :title="collapsed.has(ws.key) ? t('slackThread.sidebar.expand') : t('slackThread.sidebar.collapse')"
          @click="toggle(ws.key)"
        >
          <component
            :is="collapsed.has(ws.key) ? ChevronRight : ChevronDown"
            class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
            :stroke-width="2"
          />
          <span class="truncate">{{ ws.label ?? t('slackThread.sidebar.other') }}</span>
        </button>

        <template v-if="!collapsed.has(ws.key)">
          <div v-for="ch in ws.channels" :key="ch.key" class="mt-0.5">
            <button
              type="button"
              class="flex w-full cursor-pointer items-center gap-1 rounded py-1 pl-4 pr-1.5 text-left text-xs text-muted-foreground hover:bg-accent/50 hover:text-foreground"
              :aria-expanded="!collapsed.has(ch.key)"
              @click="toggle(ch.key)"
            >
              <component
                :is="collapsed.has(ch.key) ? ChevronRight : ChevronDown"
                class="h-3 w-3 shrink-0"
                :stroke-width="2"
              />
              <Hash v-if="ch.label" class="h-3 w-3 shrink-0" :stroke-width="2" />
              <span class="truncate font-medium">
                {{ ch.label ? ch.label.slice(1) : t('slackThread.sidebar.other') }}
              </span>
            </button>

            <ul v-if="!collapsed.has(ch.key)" class="mt-px space-y-px">
              <li v-for="th in ch.threads" :key="th.id">
                <button
                  type="button"
                  class="group flex w-full cursor-pointer items-center gap-1.5 rounded-md py-1.5 pl-8 pr-2 text-left transition-colors"
                  :class="th.id === selectedId
                    ? 'bg-primary/15 text-foreground'
                    : 'text-foreground/85 hover:bg-accent/50'"
                  :title="th.preview || th.title"
                  @click="emit('select', th.id)"
                >
                  <span
                    class="min-w-0 flex-1 truncate text-[13px]"
                    :class="th.newCount > 0 ? 'font-semibold text-foreground' : ''"
                  >
                    {{ th.title || t('slackThread.untitled') }}
                  </span>
                  <span
                    v-if="th.legacy"
                    class="shrink-0 rounded border border-border px-1 text-[9px] font-semibold uppercase text-muted-foreground"
                  >
                    {{ t('slackThread.legacy.badge') }}
                  </span>
                  <SlackStatusIcon :thread="th" :progress="progress[th.id]" />
                  <span
                    v-if="th.newCount > 0"
                    class="flex h-4 min-w-[16px] shrink-0 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-semibold leading-none text-primary-foreground"
                    :title="t('slackThread.sidebar.newCount', { count: th.newCount })"
                  >
                    {{ th.newCount }}
                  </span>
                  <span
                    class="shrink-0 text-[10px] tabular-nums text-muted-foreground/80"
                    :title="rowAgeTitle(th)"
                  >
                    {{ rowAge(th) }}
                  </span>
                </button>
              </li>
            </ul>
          </div>
        </template>
      </div>
    </div>
  </aside>
</template>
