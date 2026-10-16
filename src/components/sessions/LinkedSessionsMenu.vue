<script setup lang="ts">
// The sessions linked to a Slack thread / note / todo, in its header next to
// its project: a "Sessions" button with the linked count (muted at 0) — the
// mirror of RunView's RunLinkedKindMenu — opening one menu with two sections:
//
//   Linked sessions   rows (status dot, name, last activity): click opens the
//                     session in the main window, × on hover unlinks it
//   Link a session    search + the project's other sessions, most recently
//                     active first; click links it. Needs a project (a hint
//                     otherwise).
//
// Unlink / link keep the menu open, so several can be handled in one go. The
// project's sessions are loaded each time the menu opens.
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Link2, Search, X } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate } from '@/lib/slackThreadUtils'
import { runStatusDot, type SessionLink } from '@/lib/sessionLinks'
import { useSessionText } from '@/composables/useSessionText'
import type { RunRecord } from '@/stores/runs'

const props = defineProps<{
  /** The thread / item's project; null = nothing to link from. */
  projectId: string | null
  links: SessionLink[]
}>()

const emit = defineEmits<{
  openRun: [link: SessionLink]
  unlinkRun: [link: SessionLink]
  link: [runId: string]
}>()

const { t } = useI18n()
const { runLabel, runStatusLabel } = useSessionText()

function rowTitle(link: SessionLink): string {
  const lines = [runLabel(link), runStatusLabel(link)]
  if (link.lastActivityAt) lines.push(formatFullDate(link.lastActivityAt))
  lines.push(t('sessionLink.openHint'))
  return lines.join('\n')
}

// ── Link a session ──────────────────────────────────────────────────────────

interface Candidate {
  id: string
  label: string
  status: string
  statusLabel: string
  activity: string
}

const query = ref('')
const runs = ref<RunRecord[]>([])
const loading = ref(false)
const loadError = ref<string | null>(null)
const searchEl = ref<HTMLInputElement | null>(null)

/** The project's sessions not linked yet, most recently active first. */
const candidates = computed<Candidate[]>(() => {
  const linked = new Set(props.links.map((l) => l.runId))
  return runs.value
    .filter((r) => !linked.has(r.id))
    .map((r) => {
      const run = { title: r.title, runType: r.run_type, refNumber: r.ref_number, status: r.status }
      return {
        id: r.id,
        label: runLabel(run),
        status: r.status,
        statusLabel: runStatusLabel(run),
        activity: r.last_activity_at ?? r.created_at,
      }
    })
    .sort((a, b) => b.activity.localeCompare(a.activity))
})

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  return q ? candidates.value.filter((c) => c.label.toLowerCase().includes(q)) : candidates.value
})

/** Empty-state line of the link section, or null when there are rows. */
const linkEmpty = computed(() => {
  if (loadError.value) return null
  if (loading.value && runs.value.length === 0) return t('common.loading')
  if (runs.value.length === 0) return t('sessionLink.noSessions')
  if (candidates.value.length === 0) return t('sessionLink.allLinked')
  if (filtered.value.length === 0) return t('sessionLink.noResults')
  return null
})

let loadSeq = 0
async function load() {
  const pid = props.projectId
  if (!pid) return
  const seq = ++loadSeq
  loading.value = true
  loadError.value = null
  try {
    const list = await invoke<RunRecord[]>('list_runs', { projectId: pid })
    if (seq === loadSeq) runs.value = list
  } catch (e) {
    if (seq === loadSeq) loadError.value = String(e)
  } finally {
    if (seq === loadSeq) loading.value = false
  }
}

function onOpenChange(open: boolean) {
  if (!open) return
  query.value = ''
  if (!props.projectId) return
  void load()
  nextTick(() => searchEl.value?.focus())
}

// Another thread / project: the cached list belongs to the old one.
watch(
  () => props.projectId,
  () => {
    runs.value = []
  },
)
</script>

<template>
  <DropdownMenu align="right" @update:open="onOpenChange">
    <template #trigger>
      <Button
        variant="outline"
        size="xs"
        class="relative"
        :title="t('sessionLink.buttonTitle', links.length)"
      >
        <Link2 class="h-3 w-3" :stroke-width="1.75" />
        {{ t('sessionLink.button') }}
        <span
          class="absolute -right-1 -top-1 flex h-3.5 min-w-[14px] items-center justify-center rounded-full px-0.5 text-[9px] font-semibold leading-none"
          :class="links.length ? 'bg-primary text-primary-foreground' : 'bg-muted text-muted-foreground'"
        >
          {{ links.length }}
        </span>
      </Button>
    </template>
    <div class="w-[22rem]">
      <!-- Linked sessions -->
      <div class="px-2.5 pb-0.5 pt-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
        {{ t('sessionLink.label') }}
      </div>
      <p v-if="links.length === 0" class="px-2.5 py-1.5 text-[11px] text-muted-foreground" @click.stop>
        {{ t('sessionLink.empty') }}
      </p>
      <div v-for="link in links" :key="link.runId" class="group relative">
        <DropdownItem class="pr-7" :title="rowTitle(link)" @click="emit('openRun', link)">
          <span class="h-1.5 w-1.5 shrink-0 rounded-full" :class="runStatusDot(link.status)" />
          <span class="min-w-0 flex-1 truncate">{{ runLabel(link) }}</span>
          <span v-if="link.lastActivityAt" class="shrink-0 text-[11px] tabular-nums text-muted-foreground">
            {{ relativeAge(link.lastActivityAt) }}
          </span>
        </DropdownItem>
        <!-- Outside the row button (no nested buttons); stops so the menu stays open. -->
        <button
          type="button"
          class="absolute right-1.5 top-1/2 -translate-y-1/2 cursor-pointer rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:bg-background hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
          :title="t('sessionLink.unlink')"
          :aria-label="t('sessionLink.unlink')"
          @click.stop="emit('unlinkRun', link)"
        >
          <X class="h-3 w-3" :stroke-width="2.25" />
        </button>
      </div>

      <DropdownSeparator />

      <!-- Link a session (clicks stop here: the menu stays open) -->
      <div @click.stop>
        <div class="px-2.5 pb-1 pt-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
          {{ t('sessionLink.linkSession') }}
        </div>
        <p v-if="!projectId" class="px-2.5 pb-1.5 text-[11px] text-muted-foreground">
          {{ t('sessionLink.needProject') }}
        </p>
        <template v-else>
          <div class="relative mb-1">
            <Search
              class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
              :stroke-width="1.75"
            />
            <input
              ref="searchEl"
              v-model="query"
              type="text"
              class="h-8 w-full rounded border border-border bg-background pl-7 pr-2 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring"
              :placeholder="t('sessionLink.searchSessions')"
            >
          </div>
          <div class="max-h-48 overflow-y-auto">
            <p v-if="loadError" class="px-2.5 py-2 text-center text-[11px] text-destructive">
              {{ t('sessionLink.loadFailed', { error: loadError }) }}
            </p>
            <p v-else-if="linkEmpty" class="px-2.5 py-2 text-center text-[11px] text-muted-foreground">
              {{ linkEmpty }}
            </p>
            <DropdownItem
              v-for="c in filtered"
              v-else
              :key="c.id"
              :title="c.statusLabel"
              @click="emit('link', c.id)"
            >
              <span class="h-1.5 w-1.5 shrink-0 rounded-full" :class="runStatusDot(c.status)" />
              <span class="min-w-0 flex-1 truncate">{{ c.label }}</span>
              <span class="shrink-0 text-[11px] tabular-nums text-muted-foreground">
                {{ relativeAge(c.activity) }}
              </span>
            </DropdownItem>
          </div>
        </template>
      </div>
    </div>
  </DropdownMenu>
</template>
