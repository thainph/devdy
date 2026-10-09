<script setup lang="ts">
// The detail window for ONE capture — a Slack thread or a web page pushed in by
// the Chrome extension (opened via lib/captureWindow, `?captureWindow=1&id=…`).
// It is the capture twin of the note editor window (views/ItemWindow.vue),
// sharing its chrome: slim titlebar with pin + reload, a read view that flips
// into a content editor, and an action bar.
//
// Layout, top to bottom:
//   • the title as a large heading — THE one place a capture is renamed (click
//     or F2 → inline field; Enter / blur save, Esc cancels); web excerpts carry
//     an "Excerpt" badge;
//   • one compact metadata line (Slack: `#channel · workspace · 2h`, web:
//     `domain · author · 2h`; full dates in its tooltip) and a files chip;
//     Open in Slack / Open original is an icon in the titlebar, the project
//     picker sits on the left of the footer;
//   • the attachments as a thumbnail strip (images via the asset protocol, any
//     file opens with the OS default app);
//   • web only: a collapsible outline of the page's headings (click = scroll);
//   • the content as markdown, or the content editor in Edit mode.
//
// It is its own webview, so it reads the capture straight from the DB and
// refetches on the backend's `captures://changed` event and on focus (the MCP
// sidecar writes the DB without emitting a Tauri event).
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { convertFileSrc } from '@tauri-apps/api/core'
import { openPath, openUrl } from '@tauri-apps/plugin-opener'
import {
  ChevronDown,
  ChevronRight,
  FileText,
  Globe,
  Paperclip,
  PencilLine,
  Slack,
} from 'lucide-vue-next'
import {
  Badge,
  Button,
  ConfirmModal,
  Textarea,
  ToastHost,
} from '@/components/ui'
import InlineRenameInput from '@/components/InlineRenameInput.vue'
import DetailWindowTitlebar from '@/components/DetailWindowTitlebar.vue'
import DetailWindowFooter from '@/components/DetailWindowFooter.vue'
import CopyMarkdownButton from '@/components/CopyMarkdownButton.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { useMarkdown } from '@/lib/markdown'
import { relativeAge } from '@/lib/itemText'
import { refreshTray } from '@/lib/tray'
import { useProjectsStore } from '@/stores/projects'
import {
  CAPTURES_CHANGED,
  captureDomain,
  captureOriginalUrl,
  channelLabel,
  useCapturesStore,
  type CaptureAttachment,
  type CaptureKind,
  type CaptureDetail,
  type CapturesChanged,
} from '@/stores/captures'

const { t } = useI18n()
const { toast } = useToast()
const { confirm, state: confirmState } = useConfirm()
const store = useCapturesStore()
const projects = useProjectsStore()
const { pinned, togglePin } = useFloatingWindow()
const { renderText, loadMarkdown } = useMarkdown()

const params = new URLSearchParams(window.location.search)
const captureId = params.get('id') ?? ''
/** The opener passes the kind so even a missing capture gets the right wording. */
const kindHint: CaptureKind = params.get('kind') === 'web' ? 'web' : 'slack'
/** Attachments shown in the collapsed strip before the "+N" tile. */
const STRIP_LIMIT = 6

const capture = ref<CaptureDetail | null>(null)
const loading = ref(true)
const reloading = ref(false)
const missing = ref(false)
const editing = ref(false)
const saving = ref(false)
const renaming = ref(false)
const showAllFiles = ref(false)
const attachmentsEl = ref<HTMLElement | null>(null)

// Edit mode edits the content only; the title has its own inline rename.
const content = ref('')

let unlisten: UnlistenFn[] = []

const kind = computed<CaptureKind>(() => capture.value?.kind ?? kindHint)

const dirty = computed(
  () => editing.value && !!capture.value && content.value !== capture.value.content,
)

const heading = computed(() => capture.value?.title.trim() || t(`capture.${kind.value}.untitled`))

/**
 * Slack: `#channel · workspace · 2h` (received); web: `domain · author · 2h`
 * (captured) — whichever parts the export carried.
 */
const metaLine = computed(() => {
  const c = capture.value
  if (!c) return ''
  const parts =
    c.kind === 'slack'
      ? [channelLabel(c), c.workspace ?? '', relativeAge(c.createdAt)]
      : [
          c.siteName?.trim() || captureDomain(c),
          c.author ? t('capture.byAuthor', { author: c.author }) : '',
          relativeAge(c.exportedAt ?? c.createdAt),
        ]
  return parts.filter(Boolean).join(' · ')
})

/** Slack permalink / page URL, for the "Open in Slack" / "Open original" chip. */
const originalUrl = computed(() => (capture.value ? captureOriginalUrl(capture.value) : null))

/** Full dates for the metadata line's tooltip. */
const metaTooltip = computed(() => {
  const th = capture.value
  if (!th) return ''
  const lines = []
  if (th.kind === 'web') {
    if (th.sourceUrl) lines.push(th.sourceUrl)
    if (th.publishedAt) lines.push(t('capture.publishedAt', { date: formatDate(th.publishedAt) }))
    if (th.exportedAt) lines.push(t('capture.capturedAt', { date: formatDate(th.exportedAt) }))
  } else if (th.exportedAt) {
    lines.push(t('capture.exportedAt', { date: formatDate(th.exportedAt) }))
  }
  lines.push(t('capture.receivedAt', { date: formatDate(th.createdAt) }))
  if (th.updatedAt !== th.createdAt) {
    lines.push(t('capture.updatedAt', { date: formatDate(th.updatedAt) }))
  }
  return lines.join('\n')
})

const visibleFiles = computed(() => {
  const all = capture.value?.attachments ?? []
  return showAllFiles.value ? all : all.slice(0, STRIP_LIMIT)
})
const hiddenFileCount = computed(() =>
  showAllFiles.value ? 0 : Math.max(0, (capture.value?.attachments.length ?? 0) - STRIP_LIMIT),
)

// The OS title doubles as this window's row in the menu-bar switcher (lib/tray.ts).
const windowTitle = computed(() => t(`capture.${kind.value}.windowTitle`, { name: heading.value }))
watch(
  windowTitle,
  async (value) => {
    document.title = value
    try {
      await getCurrentWindow().setTitle(value)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

function resetDraft() {
  content.value = capture.value?.content ?? ''
}

/**
 * (Re)load the capture from the DB. Unsaved edits in progress are never
 * clobbered: only the stored copy is refreshed, the draft stays as typed.
 */
async function load(opts: { quiet?: boolean } = {}) {
  if (!captureId) {
    missing.value = true
    loading.value = false
    return
  }
  if (opts.quiet) reloading.value = true
  try {
    capture.value = await store.getDetail(captureId)
    missing.value = false
    if (!dirty.value) resetDraft()
  } catch {
    // get_capture fails when the row no longer exists.
    capture.value = null
    missing.value = true
  } finally {
    loading.value = false
    reloading.value = false
  }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`
  const mb = kb / 1024
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`
}

function formatDate(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString()
}

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|svg|avif)$/i
function isImage(att: CaptureAttachment): boolean {
  return att.mime ? att.mime.startsWith('image/') : IMAGE_EXT.test(att.name)
}

function assetUrl(att: CaptureAttachment): string {
  return convertFileSrc(att.filePath)
}

/** Base name for a tile caption (the full relative path is in the tooltip). */
function baseName(name: string): string {
  return name.split(/[\\/]/).pop() || name
}

/**
 * The markdown with links to bundled attachments (`![](attachments/x.png)`,
 * `[log](./attachments/log.txt)`) re-pointed at the stored files, so images
 * render inline and links can be opened.
 */
const renderedContent = computed(() => {
  const th = capture.value
  if (!th) return ''
  let md = th.content
  for (const att of th.attachments) {
    const url = assetUrl(att)
    for (const ref of new Set([att.name, `./${att.name}`, encodeURI(att.name)])) {
      md = md.split(`](${ref})`).join(`](${url})`)
    }
  }
  return renderText(md)
})

async function openAttachment(att: CaptureAttachment) {
  try {
    await openPath(att.filePath)
  } catch (e) {
    toast.error(t('capture.openFileFailed', { error: String(e) }))
  }
}

async function openOriginal() {
  const url = originalUrl.value
  if (!url) return
  try {
    await openUrl(url)
  } catch (e) {
    toast.error(String(e))
  }
}

// ── Outline (web) ───────────────────────────────────────────────────────────
// Built from the RENDERED headings (h1–h6 inside the markdown container), not
// from the markdown source: whatever markdown-it turns into a heading (ATX,
// setext, …) is exactly what the outline lists, so an entry can never point at
// the wrong heading. Rescanned after every render (content change, reload, or
// coming back from Edit, which remounts the container).

interface OutlineEntry {
  level: number
  text: string
  el: HTMLElement
}

const outline = shallowRef<OutlineEntry[]>([])
const outlineOpen = ref(false)
const contentEl = ref<HTMLElement | null>(null)
const outlineMinLevel = computed(() => Math.min(...outline.value.map((o) => o.level)))

function scanOutline() {
  const root = contentEl.value
  if (!root || capture.value?.kind !== 'web') {
    outline.value = []
    return
  }
  outline.value = Array.from(root.querySelectorAll<HTMLElement>('h1, h2, h3, h4, h5, h6'))
    .map((el) => ({ level: Number(el.tagName.slice(1)), text: el.textContent?.trim() ?? '', el }))
    .filter((entry) => entry.text)
}

watch([renderedContent, contentEl], scanOutline, { flush: 'post' })

function scrollToHeading(entry: OutlineEntry) {
  entry.el.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

function scrollToFiles() {
  attachmentsEl.value?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
}

// Links inside the rendered markdown must not navigate the webview: web links
// go to the OS browser, attachment links to the OS default app.
function onContentClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (/^(https?:|mailto:)/i.test(href) && !href.startsWith(convertFileSrc(''))) {
    openUrl(href).catch(() => {
      /* opener unavailable */
    })
    return
  }
  const att = capture.value?.attachments.find((a) => assetUrl(a) === href)
  if (att) void openAttachment(att)
}

/** The footer's project picker writes straight away; a failure leaves it as stored. */
async function onProjectChange(next: string | null) {
  if (!capture.value || next === capture.value.projectId) return
  try {
    await store.setProject(captureId, next)
    capture.value.projectId = next
  } catch (e) {
    toast.error(String(e))
  }
}

// ── Rename (the heading) ────────────────────────────────────────────────────

function startRename() {
  if (!capture.value) return
  renaming.value = true
}

/** Empty or unchanged → just close the field, no request. */
async function commitRename(value: string) {
  renaming.value = false
  if (!capture.value || !value || value === capture.value.title) return
  try {
    capture.value = { ...capture.value, ...(await store.rename(captureId, value)) }
  } catch (e) {
    toast.error(t('capture.renameFailed', { error: String(e) }))
  }
}

// ── Edit content ────────────────────────────────────────────────────────────

const editorRef = ref<InstanceType<typeof Textarea> | null>(null)

function startEdit() {
  if (!capture.value) return
  renaming.value = false
  resetDraft()
  editing.value = true
  nextTick(() => (editorRef.value?.$el as HTMLElement | undefined)?.focus())
}

function cancelEdit() {
  editing.value = false
  resetDraft()
}

async function save() {
  if (!capture.value || saving.value) return
  if (!dirty.value) {
    editing.value = false
    return
  }
  saving.value = true
  try {
    capture.value = {
      ...capture.value,
      ...(await store.update(captureId, { content: content.value })),
    }
    editing.value = false
    resetDraft()
    toast.success(t('item.saved'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function remove() {
  if (!capture.value) return
  if (
    !(await confirm({
      title: t(`capture.${kind.value}.confirm.deleteTitle`),
      message: t(`capture.${kind.value}.confirm.deleteMessage`),
      confirmLabel: t('common.delete'),
    }))
  ) {
    return
  }
  try {
    await store.removeMany([captureId])
  } catch (e) {
    toast.error(String(e))
    return
  }
  getCurrentWindow()
    .close()
    .catch(() => {
      /* already closing */
    })
}

/** Copy ID just fired, for its Check-icon feedback (Copy markdown has its own). */
const copiedId = ref(false)
async function copyId() {
  try {
    await navigator.clipboard.writeText(captureId)
    copiedId.value = true
    setTimeout(() => {
      copiedId.value = false
    }, 1500)
    toast.success(t('item.idCopied'))
  } catch {
    /* clipboard unavailable */
  }
}

// ── Shortcuts ───────────────────────────────────────────────────────────────
// E = edit content, F2 = rename, ⌘/Ctrl+Enter = save, Esc = cancel the edit.
// The rename field handles its own Enter / Esc. Nothing fires mid-composition
// (IME), and the letter shortcut never fires while typing in a field.

function isTypingTarget(el: EventTarget | null): boolean {
  const node = el as HTMLElement | null
  if (!node) return false
  return node.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(node.tagName)
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (!capture.value || missing.value || confirmState.open) return
  if (editing.value) {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault()
      void save()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      cancelEdit()
    }
    return
  }
  if (renaming.value || isTypingTarget(e.target)) return
  if (e.key === 'F2') {
    e.preventDefault()
    startRename()
  } else if (e.key.toLowerCase() === 'e' && !e.metaKey && !e.ctrlKey && !e.altKey) {
    e.preventDefault()
    startEdit()
  }
}

onMounted(async () => {
  loadMarkdown()
  projects.fetchProjects()
  document.addEventListener('keydown', onKeydown)
  await load()
  try {
    unlisten.push(
      await listen<CapturesChanged>(CAPTURES_CHANGED, (e) => {
        if (e.payload?.id && e.payload.id !== captureId) return
        if (e.payload?.action === 'deleted') {
          capture.value = null
          missing.value = true
        } else {
          void load({ quiet: true })
        }
      }),
      // MCP writes don't emit a Tauri event: catch up whenever the window is focused.
      await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (focused && !missing.value) void load({ quiet: true })
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
})

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKeydown)
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div class="flex h-screen w-screen flex-col bg-background text-foreground overflow-hidden">
    <!-- Slim titlebar: which capture this is and whether it has unsaved changes. -->
    <DetailWindowTitlebar
      :icon="kind === 'web' ? Globe : Slack"
      :title="heading"
      :dirty="dirty"
      :pinned="pinned"
      :reloading="reloading"
      :external-label="originalUrl ? (kind === 'web' ? t('capture.openOriginal') : t('capture.openInSlack')) : undefined"
      @toggle-pin="togglePin"
      @reload="load({ quiet: true })"
      @open-external="openOriginal"
    />

    <!-- Body -->
    <div class="min-h-0 flex-1 flex flex-col overflow-auto px-4 py-3">
      <div v-if="loading" class="space-y-2">
        <div class="h-8 animate-pulse rounded-md border border-border bg-card" />
        <div class="h-48 animate-pulse rounded-md border border-border bg-card" />
      </div>

      <p v-else-if="missing || !capture" class="p-4 text-sm text-muted-foreground">
        {{ t(`capture.${kind}.missing`) }}
      </p>

      <template v-else>
        <!-- Title: the one place a capture is renamed (click or F2). -->
        <div class="flex shrink-0 items-center gap-1.5">
          <InlineRenameInput
            v-if="renaming"
            class="w-full text-lg font-semibold leading-snug"
            :value="capture.title"
            :placeholder="t(`capture.${kind}.renamePlaceholder`)"
            @commit="commitRename"
            @cancel="renaming = false"
          />
          <template v-else>
            <h1
              class="min-w-0 cursor-text rounded px-0.5 -mx-0.5 text-lg font-semibold leading-snug break-words transition-colors hover:bg-accent/50"
              :title="t('capture.renameHint')"
              @click="startRename"
            >
              {{ heading }}
            </h1>
            <!-- Renamed by the user: a re-export of the capture keeps this title. -->
            <span
              v-if="capture.titleCustom"
              class="shrink-0 text-muted-foreground/60"
              :title="t('capture.renamedHint')"
              :aria-label="t('capture.renamedHint')"
            >
              <PencilLine class="h-3.5 w-3.5" :stroke-width="1.75" />
            </span>
            <!-- Web: only the user's selection was captured, not the whole page. -->
            <Badge
              v-if="capture.kind === 'web' && capture.selection"
              tone="info"
              size="xs"
              class="shrink-0"
              :title="t('capture.excerptHint')"
            >
              {{ t('capture.excerpt') }}
            </Badge>
          </template>
        </div>

        <!-- Compact metadata: source · age (full dates in the tooltip). -->
        <p
          class="mt-1 shrink-0 truncate text-xs text-muted-foreground"
          :title="metaTooltip"
        >
          {{ metaLine }}
        </p>

        <!-- Files chip (jumps to the strip). The project picker is in the footer,
             Open in Slack / Open original in the titlebar. -->
        <div v-if="capture.attachments.length" class="mt-2 flex shrink-0 flex-wrap items-center gap-1.5">
          <Button
            variant="outline"
            size="xs"
            @click="scrollToFiles"
          >
            <Paperclip class="h-3 w-3" :stroke-width="1.75" />
            {{ t('capture.filesChip', { count: capture.attachments.length }, capture.attachments.length) }}
          </Button>
        </div>

        <!-- Attachments: a thumbnail strip, "+N" expands it into the full grid. -->
        <div
          v-if="capture.attachments.length"
          ref="attachmentsEl"
          class="mt-3 shrink-0"
          :aria-label="t('capture.attachments', { count: capture.attachments.length })"
        >
          <ul class="flex gap-2" :class="showAllFiles ? 'flex-wrap' : 'overflow-x-auto pb-1'">
            <li v-for="att in visibleFiles" :key="att.id" class="shrink-0">
              <button
                type="button"
                class="flex h-20 w-20 cursor-pointer flex-col overflow-hidden rounded-md border border-border/70 bg-card text-left transition-colors hover:border-border hover:bg-accent/40"
                :title="`${t('capture.openFile', { name: att.name })} · ${formatSize(att.size)}`"
                @click="openAttachment(att)"
              >
                <img
                  v-if="isImage(att)"
                  :src="assetUrl(att)"
                  :alt="att.name"
                  class="h-full w-full object-cover"
                  loading="lazy"
                >
                <template v-else>
                  <span class="flex flex-1 items-center justify-center bg-muted/40">
                    <FileText class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
                  </span>
                  <span class="truncate px-1.5 py-1 text-[10px] text-foreground">{{ baseName(att.name) }}</span>
                </template>
              </button>
            </li>
            <li v-if="hiddenFileCount > 0" class="shrink-0">
              <button
                type="button"
                class="flex h-20 w-20 cursor-pointer items-center justify-center rounded-md border border-dashed border-border bg-muted/30 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                :title="t('capture.showAllFiles')"
                @click="showAllFiles = true"
              >
                +{{ hiddenFileCount }}
              </button>
            </li>
          </ul>
          <button
            v-if="showAllFiles && capture.attachments.length > STRIP_LIMIT"
            type="button"
            class="mt-1 cursor-pointer text-[11px] text-muted-foreground hover:text-foreground"
            @click="showAllFiles = false"
          >
            {{ t('capture.showLessFiles') }}
          </button>
        </div>

        <div class="my-3 h-px shrink-0 bg-border/60" />

        <!-- Web: collapsible outline of the page's headings (click = scroll). -->
        <div v-if="!editing && outline.length > 1" class="mb-3 shrink-0">
          <button
            type="button"
            class="flex cursor-pointer items-center gap-1 text-[11px] font-medium uppercase tracking-wider text-muted-foreground hover:text-foreground"
            :aria-expanded="outlineOpen"
            @click="outlineOpen = !outlineOpen"
          >
            <component :is="outlineOpen ? ChevronDown : ChevronRight" class="h-3 w-3" :stroke-width="2" />
            {{ t('capture.outline', { count: outline.length }) }}
          </button>
          <ul v-if="outlineOpen" class="mt-1.5 space-y-0.5 border-l border-border/60 pl-2">
            <li v-for="(entry, i) in outline" :key="i">
              <button
                type="button"
                class="block w-full cursor-pointer truncate rounded px-1.5 py-0.5 text-left text-xs text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground"
                :style="{ paddingLeft: `${(entry.level - outlineMinLevel) * 12 + 6}px` }"
                :title="entry.text"
                @click="scrollToHeading(entry)"
              >
                {{ entry.text }}
              </button>
            </li>
          </ul>
        </div>

        <!-- Edit: the markdown body only (the title is renamed above). -->
        <template v-if="editing">
          <Textarea
            ref="editorRef"
            v-model="content"
            class="min-h-[12rem] flex-1 font-mono leading-relaxed"
            :placeholder="t('item.notePlaceholder')"
          />
          <p class="mt-2 shrink-0 text-[11px] text-muted-foreground">
            {{ t('item.hint', { save: '⌘/Ctrl+Enter', esc: 'Esc' }) }}
          </p>
        </template>

        <!-- Read: the conversation as markdown -->
        <template v-else>
          <div
            v-if="capture.content.trim()"
            ref="contentEl"
            class="markdown-output text-sm"
            @click="onContentClick"
            v-html="renderedContent"
          />
          <p v-else class="text-sm text-muted-foreground italic">{{ t('item.empty') }}</p>
        </template>
      </template>
    </div>

    <!-- Actions (shared with the note / todo window). Reading: project on the
         left; Copy markdown · Edit · ⋯ (Copy ID, Delete) on the right.
         Editing: Cancel · Save. -->
    <DetailWindowFooter
      v-if="capture && !missing"
      :editing="editing"
      :can-save="!saving"
      :copied-id="copiedId"
      @edit="startEdit"
      @cancel="cancelEdit"
      @save="save"
      @copy-id="copyId"
      @delete="remove"
    >
      <template #left>
        <ProjectSelect :model-value="capture.projectId" @update:model-value="onProjectChange" />
      </template>
      <template #actions>
        <CopyMarkdownButton :text="capture.content" />
      </template>
    </DetailWindowFooter>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
