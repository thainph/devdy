<script setup lang="ts">
// GitHub Mentions: every place on GitHub waiting for the user's answer (a
// direct @mention, a team mention, a reply on a thread they take part in),
// across all linked GitHub accounts. Header layout follows the PR Inbox:
// search + Filters / Display beside Refresh. Mentions answered in the last
// week sit in a collapsed section at the bottom.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Banner, Button, ContextMenu, DropdownItem, DropdownSeparator, Input, PageHeader, Skeleton } from '@/components/ui'
import MentionCard from '@/components/githubMentions/MentionCard.vue'
import MentionFiltersPopover from '@/components/githubMentions/MentionFiltersPopover.vue'
import MentionDisplayPopover from '@/components/githubMentions/MentionDisplayPopover.vue'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { useToast } from '@/composables/useToast'
import {
  activeFilters,
  applyFilters,
  filterOptions,
  groupMentions,
  MENTION_REASONS,
  splitAnswered,
  type MentionFilterDimension,
  type MentionFilterOption,
  type MentionItem,
} from '@/lib/githubMentions'
import {
  AtSign, Check, ChevronRight, Copy, EyeOff, ExternalLink, MailOpen, RefreshCw, Search, SearchX, X,
} from 'lucide-vue-next'

const { t } = useI18n()
const { toast } = useToast()
const store = useGithubMentionsStore()

const search = ref('')
const showAnswered = ref(false)
const collapsed = ref<Set<string>>(new Set())

onMounted(() => { void store.load() })

const options = computed(() => filterOptions(store.items))
const effective = computed(() => activeFilters(store.filters, store.items))
const activeCount = computed(() => Object.values(effective.value).reduce((n, v) => n + v.length, 0))
const visible = computed(() => applyFilters(store.items, effective.value, search.value))
const split = computed(() => splitAnswered(visible.value))
const groups = computed(() => groupMentions(split.value.pending, store.groupBy))

function optionLabel(d: MentionFilterDimension, o: MentionFilterOption): string {
  if (d === 'reasons' && (MENTION_REASONS as string[]).includes(o.value)) return t(`githubMentions.reason.${o.value}`)
  return o.label
}

const chips = computed(() =>
  (Object.keys(effective.value) as MentionFilterDimension[]).flatMap(d =>
    effective.value[d].map(value => {
      const o = options.value[d].find(x => x.value === value)
      return { key: `${d}:${value}`, dimension: d, value, title: t(`githubMentions.filters.${d === 'reasons' ? 'reason' : d === 'accounts' ? 'account' : 'repo'}`), label: o ? optionLabel(d, o) : value }
    }),
  ),
)

function removeChip(d: MentionFilterDimension, value: string) {
  store.setFilter(d, store.filters[d].filter(v => v !== value))
}

function toggleGroup(key: string) {
  const next = new Set(collapsed.value)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  collapsed.value = next
}

// Card menu (right-click). Teleported ContextMenu, like the PR Inbox.
const menu = ref<{ open: boolean; x: number; y: number; item: MentionItem | null }>({ open: false, x: 0, y: 0, item: null })
function openMenu(item: MentionItem, x: number, y: number) {
  menu.value = { open: true, x, y, item }
}
function closeMenu() {
  menu.value = { ...menu.value, open: false }
}

async function menuAction(action: 'open' | 'read' | 'copy' | 'dismiss') {
  const item = menu.value.item
  closeMenu()
  if (!item) return
  const url = item.trigger?.html_url || item.html_url
  try {
    if (action === 'open') await openUrl(url)
    else if (action === 'read') await store.markRead(item.key)
    else if (action === 'copy') {
      await navigator.clipboard.writeText(url)
      toast.success(t('githubMentions.menu.copied'))
    } else {
      await store.dismiss(item.key)
      toast.success(t('githubMentions.menu.dismissed'))
    }
  } catch (e) {
    toast.error(String(e))
  }
}
</script>

<template>
  <div class="flex h-full flex-col">
    <PageHeader
      :title="t('githubMentions.title')"
      :count="store.items.length > 0 ? split.pending.length : null"
      :count-title="t('githubMentions.pendingTitle')"
    >
      <template #leading>
        <AtSign class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
      </template>
      <template #actions>
        <template v-if="store.items.length > 0">
          <div class="relative">
            <Search class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
            <Input
              v-model="search"
              size="sm"
              class="w-56 pl-7 pr-7"
              :placeholder="t('githubMentions.searchPlaceholder')"
              :aria-label="t('githubMentions.searchPlaceholder')"
              @keydown.esc="search = ''"
            />
            <button
              v-if="search"
              type="button"
              class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
              :title="t('githubMentions.clearSearch')"
              :aria-label="t('githubMentions.clearSearch')"
              @click="search = ''"
            >
              <X class="h-3.5 w-3.5" :stroke-width="2" />
            </button>
          </div>
          <MentionFiltersPopover
            :options="options"
            :model-value="store.filters"
            :active-count="activeCount"
            :option-label="optionLabel"
            @update="store.setFilter"
            @clear="store.clearFilters()"
          />
          <MentionDisplayPopover />
        </template>
        <Button variant="outline" :disabled="store.loading" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="1.75" />
          {{ t('common.refresh') }}
        </Button>
      </template>
    </PageHeader>

    <div v-if="chips.length > 0" class="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-border/60 px-6 py-2">
      <span
        v-for="chip in chips"
        :key="chip.key"
        class="inline-flex max-w-full items-center gap-1 rounded-md bg-muted py-1 pl-2 pr-1 text-[11px] text-foreground"
      >
        <span class="shrink-0 text-muted-foreground">{{ chip.title }}:</span>
        <span class="min-w-0 max-w-40 truncate" :title="chip.label">{{ chip.label }}</span>
        <button
          type="button"
          class="shrink-0 rounded p-0.5 cursor-pointer text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          :title="t('githubMentions.filters.remove', { filter: `${chip.title}: ${chip.label}` })"
          :aria-label="t('githubMentions.filters.remove', { filter: `${chip.title}: ${chip.label}` })"
          @click="removeChip(chip.dimension, chip.value)"
        >
          <X class="h-3 w-3" :stroke-width="2" />
        </button>
      </span>
      <button type="button" class="ml-1 cursor-pointer text-[11px] font-medium text-primary hover:underline" @click="store.clearFilters()">
        {{ t('githubMentions.filters.clearAll') }}
      </button>
    </div>

    <div class="flex-1 overflow-auto p-6">
      <Banner v-if="store.error" tone="destructive" class="mb-4">{{ store.error }}</Banner>
      <Banner v-if="store.warnings.length > 0" tone="warning" class="mb-4">
        <p class="font-medium text-warning">{{ t('githubMentions.partialFailTitle', store.warnings.length) }}</p>
        <p class="mt-0.5 text-muted-foreground">
          {{ store.warnings.map(w => `${w.account_label}: ${w.message}`).join(' · ') }}
        </p>
      </Banner>

      <div v-if="store.loading && store.items.length === 0" class="flex flex-col gap-2">
        <Skeleton v-for="i in 5" :key="i" class="h-20 rounded-lg" />
      </div>

      <div
        v-else-if="store.items.length === 0 && !store.error"
        class="flex h-full min-h-80 flex-col items-center justify-center text-center"
      >
        <div class="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
          <AtSign class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('githubMentions.emptyTitle') }}</p>
        <p class="mt-1 max-w-72 text-xs text-muted-foreground">{{ t('githubMentions.emptyHint') }}</p>
      </div>

      <div v-else class="flex flex-col gap-5">
        <div
          v-if="visible.length === 0"
          class="flex items-center gap-2 rounded-lg border border-dashed border-border/60 px-3 py-3 text-xs text-muted-foreground"
        >
          <SearchX class="h-4 w-4 shrink-0" :stroke-width="1.75" />
          <span>{{ t('githubMentions.noMatch') }}</span>
          <span class="text-muted-foreground/50">·</span>
          <button type="button" class="cursor-pointer font-medium text-primary hover:underline" @click="search = ''; store.clearFilters()">
            {{ t('githubMentions.clearSearchAndFilters') }}
          </button>
        </div>

        <p v-else-if="split.pending.length === 0" class="flex items-center gap-2 text-xs text-muted-foreground">
          <Check class="h-4 w-4 text-success" :stroke-width="2" />
          {{ t('githubMentions.allAnswered') }}
        </p>

        <section v-for="g in groups" :key="g.key" class="flex flex-col gap-2">
          <button
            v-if="store.groupBy !== 'none'"
            type="button"
            class="flex min-w-0 items-center gap-2 text-left text-xs font-semibold cursor-pointer"
            :aria-expanded="!collapsed.has(g.key)"
            @click="toggleGroup(g.key)"
          >
            <ChevronRight
              class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
              :class="{ 'rotate-90': !collapsed.has(g.key) }"
              :stroke-width="2"
            />
            <PrAvatar v-if="store.groupBy === 'person'" :url="g.avatarUrl" :name="g.label || '?'" :size="16" />
            <span class="min-w-0 truncate" :class="{ 'font-mono font-medium': store.groupBy === 'repo' }">{{ g.label || t('githubMentions.unknownAuthor') }}</span>
            <span class="shrink-0 font-normal text-[11px] text-muted-foreground">{{ t('githubMentions.groupPending', g.items.length) }}</span>
          </button>
          <template v-if="!collapsed.has(g.key)">
            <MentionCard v-for="item in g.items" :key="item.key" :item="item" @menu="openMenu" />
          </template>
        </section>

        <section v-if="split.answered.length > 0" class="flex flex-col gap-2">
          <button
            type="button"
            class="flex items-center gap-2 rounded-lg border border-dashed border-border px-3 py-2 text-xs text-muted-foreground cursor-pointer transition-colors hover:text-foreground"
            :aria-expanded="showAnswered"
            @click="showAnswered = !showAnswered"
          >
            <Check class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('githubMentions.answeredSection', { n: split.answered.length }) }}
            <ChevronRight class="ml-auto h-3.5 w-3.5 transition-transform" :class="{ 'rotate-90': showAnswered }" :stroke-width="2" />
          </button>
          <template v-if="showAnswered">
            <MentionCard v-for="item in split.answered" :key="item.key" :item="item" @menu="openMenu" />
          </template>
        </section>
      </div>
    </div>

    <ContextMenu :open="menu.open" :x="menu.x" :y="menu.y" @close="closeMenu">
      <DropdownItem @click="menuAction('open')">
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.openOnGithub') }}
      </DropdownItem>
      <DropdownItem v-if="menu.item?.unread" @click="menuAction('read')">
        <MailOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.markRead') }}
      </DropdownItem>
      <DropdownItem @click="menuAction('copy')">
        <Copy class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.copyLink') }}
      </DropdownItem>
      <DropdownSeparator />
      <DropdownItem @click="menuAction('dismiss')">
        <EyeOff class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('githubMentions.menu.dismiss') }}
      </DropdownItem>
    </ContextMenu>
  </div>
</template>
