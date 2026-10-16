<script setup lang="ts">
// Right pane of the Slack threads window: header, status banners, then the
// thread itself — the parent message, an "N replies" divider, the replies
// (Slack-style grouping of consecutive messages), and a "New" divider before the
// replies that arrived since the thread was last seen. A legacy snapshot that
// has no fetched messages yet renders its old markdown + files instead.
//
// Refetches keep the scroll position; when the reader is at the bottom, new
// messages scroll into view. The image lightbox lives here (it steps through
// every image of the thread).
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { convertFileSrc } from '@tauri-apps/api/core'
import { openPath, openUrl } from '@tauri-apps/plugin-opener'
import { useMarkdown } from '@/lib/markdown'
import type { MrkdwnContext } from '@/lib/slackMrkdwn'
import { groupMessages } from '@/lib/slackThreadUtils'
import { isBusy, type SlackFile, type SlackThreadProgress, type SlackThreadView } from '@/stores/slackThreads'
import SlackThreadHeader from './SlackThreadHeader.vue'
import SlackThreadBanner from './SlackThreadBanner.vue'
import SlackMessage from './SlackMessage.vue'
import SlackMessageFiles from './SlackMessageFiles.vue'
import SlackImageLightbox from './SlackImageLightbox.vue'

const props = defineProps<{
  view: SlackThreadView
  progress: SlackThreadProgress | null
  /** Window-wide "Show original" toggle. */
  showOriginal: boolean
  /** New-message count when the thread was opened (before it was marked seen). */
  newCount: number
  renaming: boolean
  refreshing: boolean
}>()

const emit = defineEmits<{
  startRename: []
  commitRename: [title: string]
  cancelRename: []
  toggleOriginal: []
  refresh: []
  openInSlack: []
  openMcp: []
}>()

const { t } = useI18n()
const { renderText, loadMarkdown } = useMarkdown()
loadMarkdown()

const scroller = ref<HTMLElement | null>(null)

const ctx = computed<MrkdwnContext>(() => ({
  users: props.view.users,
  channels: props.view.channels,
  workspaceDomain: props.view.workspaceDomain,
}))

const rows = computed(() => groupMessages(props.view.messages))
const replyCount = computed(() => rows.value.filter((r) => !r.isParent).length)
const translating = computed(() => props.view.fetchStatus === 'translating')

/** Index (in rows) of the first new reply, or -1. */
const firstNewIndex = computed(() => {
  if (props.newCount <= 0 || props.newCount > replyCount.value) return -1
  return rows.value.length - props.newCount
})

const filesById = computed(() => new Map(props.view.files.map((f) => [f.id, f])))

function filesOf(fileIds: string[]): SlackFile[] {
  return fileIds.map((id) => filesById.value.get(id)).filter((f): f is SlackFile => !!f)
}

// ── Per-message original / translation override (resets per thread) ─────────

const flipped = ref<Set<string>>(new Set())

function toggleMessage(ts: string) {
  const next = new Set(flipped.value)
  if (next.has(ts)) next.delete(ts)
  else next.add(ts)
  flipped.value = next
}

function showOriginalFor(ts: string): boolean {
  return props.showOriginal !== flipped.value.has(ts)
}

// ── Legacy snapshot ─────────────────────────────────────────────────────────

const showLegacy = computed(() => props.view.legacy && props.view.messages.length === 0)
const legacyFiles = computed(() => props.view.files.filter((f) => f.slackFileId === null))

/** The old markdown with bundled-file links re-pointed at the stored files. */
const legacyHtml = computed(() => {
  let md = props.view.legacyContent ?? ''
  for (const f of legacyFiles.value) {
    if (!f.filePath) continue
    const url = convertFileSrc(f.filePath)
    for (const ref of new Set([f.name, `./${f.name}`, encodeURI(f.name)])) {
      md = md.split(`](${ref})`).join(`](${url})`)
    }
  }
  return renderText(md)
})

function onLegacyClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  const file = legacyFiles.value.find((f) => f.filePath && convertFileSrc(f.filePath) === href)
  if (file?.filePath) {
    openPath(file.filePath).catch(() => {
      /* opener unavailable */
    })
  } else if (/^(https?:|mailto:)/i.test(href)) {
    openUrl(href).catch(() => {
      /* opener unavailable */
    })
  }
}

// ── Lightbox ────────────────────────────────────────────────────────────────

const images = computed(() => {
  const ordered: SlackFile[] = []
  const seen = new Set<string>()
  const push = (f: SlackFile | undefined) => {
    if (f && f.isImage && f.filePath && !seen.has(f.id)) {
      seen.add(f.id)
      ordered.push(f)
    }
  }
  for (const m of props.view.messages) m.fileIds.forEach((id) => push(filesById.value.get(id)))
  props.view.files.forEach(push)
  return ordered
})
const lightboxId = ref<string | null>(null)

// ── Scroll ──────────────────────────────────────────────────────────────────

let stickToBottom = false

watch(
  () => props.view.id,
  async () => {
    flipped.value = new Set()
    lightboxId.value = null
    await nextTick()
    if (scroller.value) scroller.value.scrollTop = 0
  },
)

// Before the DOM updates: was the reader at the bottom? After: follow new messages.
watch(
  () => props.view.messages.length,
  () => {
    const el = scroller.value
    stickToBottom = !!el && el.scrollHeight - el.scrollTop - el.clientHeight < 24
  },
  { flush: 'pre' },
)
watch(
  () => props.view.messages.length,
  (now, before) => {
    const el = scroller.value
    if (el && stickToBottom && now > (before ?? 0)) el.scrollTop = el.scrollHeight
  },
  { flush: 'post' },
)
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col">
    <SlackThreadHeader
      :thread="view"
      :renaming="renaming"
      :show-original="showOriginal"
      :refreshing="refreshing || isBusy(view.fetchStatus)"
      @start-rename="emit('startRename')"
      @commit-rename="emit('commitRename', $event)"
      @cancel-rename="emit('cancelRename')"
      @toggle-original="emit('toggleOriginal')"
      @refresh="emit('refresh')"
      @open-in-slack="emit('openInSlack')"
    />
    <SlackThreadBanner
      :thread="view"
      :progress="progress"
      @retry="emit('refresh')"
      @open-mcp="emit('openMcp')"
    />

    <div ref="scroller" class="min-h-0 flex-1 overflow-y-auto py-3">
      <!-- Legacy snapshot: the old markdown + its files until a re-fetch succeeds. -->
      <div v-if="showLegacy" class="px-5">
        <div class="markdown-output text-sm" @click="onLegacyClick" v-html="legacyHtml" />
        <SlackMessageFiles :files="legacyFiles" class="mt-3" @open-image="lightboxId = $event" />
      </div>

      <p v-else-if="rows.length === 0" class="px-5 py-6 text-sm text-muted-foreground">
        {{ t('slackThread.empty') }}
      </p>

      <template v-for="(row, i) in rows" v-else :key="row.message.ts">
        <div
          v-if="i === firstNewIndex"
          class="my-2 flex items-center gap-2 px-5 text-[11px] font-semibold text-destructive"
        >
          <span class="h-px flex-1 bg-destructive/50" />
          {{ t('slackThread.message.newDivider') }}
        </div>
        <SlackMessage
          :message="row.message"
          :continued="row.continued && i !== firstNewIndex"
          :ctx="ctx"
          :user="row.message.userId ? view.users[row.message.userId] ?? null : null"
          :files="filesOf(row.message.fileIds)"
          :show-original="showOriginalFor(row.message.ts)"
          :translating="translating"
          @toggle-original="toggleMessage(row.message.ts)"
          @open-image="lightboxId = $event"
        />
        <div
          v-if="row.isParent && replyCount > 0"
          class="my-2 flex items-center gap-3 px-5 text-xs text-muted-foreground"
        >
          <span class="shrink-0 font-medium">{{ t('slackThread.replies', { n: replyCount }, replyCount) }}</span>
          <span class="h-px flex-1 bg-border" />
        </div>
      </template>
    </div>

    <SlackImageLightbox
      v-if="lightboxId"
      :images="images"
      :current-id="lightboxId"
      @select="lightboxId = $event"
      @close="lightboxId = null"
    />
  </section>
</template>
