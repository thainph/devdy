<script setup lang="ts">
import { onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import { usePrInboxStore, prKey, type PrInboxItem } from '@/stores/prInbox'
import { useRunsStore } from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { Button, Card, Badge, StatusBadge, Skeleton } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { GitPullRequest, RefreshCw, Sparkles, FolderPlus, ExternalLink, User, Eye } from 'lucide-vue-next'

const { t } = useI18n()
const router = useRouter()
const store = usePrInboxStore()
const runsStore = useRunsStore()
const live = useLiveRunsStore()
const { toast } = useToast()

onMounted(() => {
  if (store.items.length === 0) store.refresh()
})

function openPr(item: PrInboxItem) {
  openUrl(item.html_url).catch(() => { /* opener unavailable */ })
}

// Effective status for a PR's linked review run: a live streaming status (from
// the liveRuns store) wins over the DB snapshot taken at refresh time.
function runStatus(item: PrInboxItem): string | null {
  if (!item.existing_run_id) return null
  return live.get(item.existing_run_id)?.status ?? item.existing_run_status
}

function isRunning(item: PrInboxItem): boolean {
  const s = runStatus(item)
  return s === 'running' || s === 'fetched'
}

// Open the review session already linked to this PR.
function openRun(item: PrInboxItem) {
  if (!item.project_id || !item.existing_run_id) return
  router.push(`/projects/${item.project_id}/run/${item.existing_run_id}`)
}

// Start a review, or resume the linked one. A PR that already has a review run
// (in DB or just created) never spawns a second session — we navigate to it.
async function handleReview(item: PrInboxItem) {
  if (!item.mapped || !item.project_id || !item.repo_id) return
  if (item.existing_run_id) { openRun(item); return }
  const key = prKey(item)
  if (store.reviewingKeys.has(key)) return
  store.setReviewing(key, true)
  try {
    const run = await runsStore.fetchPr(item.project_id, item.repo_id, item.number)
    await runsStore.startRun(run.id)
    // Link immediately so a second click resumes instead of duplicating. The run
    // streams in the background (liveRuns / ActiveRunsDock) — stay on this screen.
    store.linkRun(key, run.id, 'running')
    toast.success(t('prInbox.reviewing', { repo: item.repo, number: item.number }))
  } catch (e) {
    toast.error(String(e))
  } finally {
    store.setReviewing(key, false)
  }
}

function relativeTime(iso: string): string {
  const then = new Date(iso).getTime()
  const diff = Date.now() - then
  const mins = Math.round(diff / 60000)
  if (mins < 1) return t('prInbox.justNow')
  if (mins < 60) return t('prInbox.minsAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('prInbox.hoursAgo', { hours })
  const days = Math.round(hours / 24)
  if (days < 30) return t('prInbox.daysAgo', { days })
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">{{ t('prInbox.title') }}</h1>
        <span
          v-if="!store.loading && store.items.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.items.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button variant="ghost" :disabled="store.loading" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="2" />
          {{ t('common.refresh') }}
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Loading skeleton — mirrors the PR card layout so the wait reads as
           "content incoming". Rows cascade in with a small stagger. -->
      <div v-if="store.loading && store.items.length === 0" class="flex flex-col gap-3">
        <div
          v-for="(w, i) in [72, 54, 80, 60, 66]" :key="i"
          class="animate-fade-rise flex items-center gap-4 rounded-lg border border-border/60 bg-card p-4"
          :style="{ animationDelay: `${i * 70}ms` }"
        >
          <Skeleton class="h-9 w-9 shrink-0 rounded-lg" />
          <div class="min-w-0 flex-1 space-y-2.5">
            <Skeleton class="h-3.5 rounded" :style="{ width: `${w}%` }" />
            <div class="flex items-center gap-2">
              <Skeleton class="h-2.5 w-28 rounded" />
              <Skeleton class="h-2.5 w-16 rounded" />
              <Skeleton class="h-2.5 w-20 rounded" />
            </div>
          </div>
          <Skeleton class="h-7 w-24 shrink-0 rounded-md" />
        </div>
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="store.items.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <GitPullRequest class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('prInbox.emptyTitle') }}</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-64">
          {{ t('prInbox.emptyBody') }}
        </p>
        <Button class="mt-4" variant="ghost" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('common.refresh') }}
        </Button>
      </div>

      <!-- PR list -->
      <div v-else class="flex flex-col gap-3">
        <Card
          v-for="(item, i) in store.items"
          :key="`${item.account_id}:${prKey(item)}`"
          class="group relative flex flex-row items-center gap-4 animate-fade-rise transition-all duration-150 hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)]"
          :style="{ animationDelay: `${Math.min(i, 12) * 40}ms` }"
          body-class="flex flex-row items-center gap-4 flex-1 p-4"
        >
          <!-- Icon -->
          <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary">
            <GitPullRequest class="h-4.5 w-4.5" :stroke-width="1.75" />
          </div>

          <!-- Main info -->
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-1.5 min-w-0">
              <button
                class="text-sm font-semibold truncate leading-tight hover:text-primary transition-colors cursor-pointer text-left"
                :title="item.title"
                @click="openPr(item)"
              >
                {{ item.title }}
              </button>
              <ExternalLink class="h-3 w-3 shrink-0 text-muted-foreground/50 opacity-0 group-hover:opacity-100 transition-opacity" :stroke-width="2" />
            </div>
            <div class="flex items-center gap-2 flex-wrap mt-1 text-[11px] text-muted-foreground">
              <span class="font-mono truncate">{{ item.owner }}/{{ item.repo }} <span class="text-muted-foreground/70">#{{ item.number }}</span></span>
              <span class="flex items-center gap-1">
                <User class="h-3 w-3" :stroke-width="1.75" />
                {{ item.author_login }}
              </span>
              <span class="text-muted-foreground/60">{{ relativeTime(item.updated_at) }}</span>
              <Badge tone="neutral" size="xs" class="shrink-0">{{ item.account_label }}</Badge>
            </div>
          </div>

          <!-- Mapping + action -->
          <div class="flex items-center gap-2 shrink-0">
            <template v-if="item.mapped">
              <Badge tone="primary" size="xs" class="shrink-0 max-w-40 truncate" :title="item.project_name ?? ''">
                {{ item.project_name }}
              </Badge>
              <!-- Linked review session already exists → show its status + open it -->
              <template v-if="item.existing_run_id">
                <StatusBadge v-if="runStatus(item)" :status="runStatus(item)!" size="xs" />
                <Button variant="outline" @click="openRun(item)">
                  <RefreshCw v-if="isRunning(item)" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
                  <Eye v-else class="h-3.5 w-3.5" :stroke-width="2" />
                  {{ isRunning(item) ? t('prInbox.reviewingShort') : t('prInbox.viewReview') }}
                </Button>
              </template>
              <!-- No session yet → start one -->
              <Button
                v-else
                :disabled="store.reviewingKeys.has(prKey(item))"
                @click="handleReview(item)"
              >
                <Sparkles class="h-3.5 w-3.5" :stroke-width="2" />
                {{ store.reviewingKeys.has(prKey(item)) ? t('prInbox.opening') : t('prInbox.aiReview') }}
              </Button>
            </template>
            <template v-else>
              <Badge tone="neutral" size="xs" class="shrink-0">{{ t('prInbox.notInDevdy') }}</Badge>
              <Button variant="ghost" @click="router.push('/projects')">
                <FolderPlus class="h-3.5 w-3.5" :stroke-width="2" />
                {{ t('prInbox.addToDevdy') }}
              </Button>
            </template>
          </div>
        </Card>
      </div>
    </div>
  </div>
</template>
