<script setup lang="ts">
// Self-contained file viewer: loads a project file and renders it (syntax-
// highlighted text, rendered/raw markdown, images, video, audio, PDF, or an
// "open externally" fallback). Used both inside the in-app modal (RunView) and
// in the standalone pop-out window (FileViewerWindow). Hosts supply chrome-
// specific buttons (full-screen, pop-out, close) via the #actions slot.
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import {
  FileCode2, AArrowDown, AArrowUp, ExternalLink, Copy, FileQuestion, FileWarning, FolderOpen, RotateCw, Code2, ClipboardCopy, Check, Languages, Pencil, Save, X,
} from 'lucide-vue-next'
import { convertFileSrc } from '@tauri-apps/api/core'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import type MarkdownIt from 'markdown-it'
import { Button } from '@/components/ui'
import TranslatePopover from '@/components/TranslatePopover.vue'
import { useRunsStore } from '@/stores/runs'
import { useProjectsStore } from '@/stores/projects'
import { useAppSettingsStore } from '@/stores/appSettings'
import { applyMermaidFence, vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { matchProjectFile, parseLineRef, decorateFileLinks } from '@/lib/fileLinks'

const props = withDefaults(defineProps<{
  projectPath: string
  path: string
  line?: number | null
}>(), {
  line: null,
})

const emit = defineEmits<{
  /** A file link inside the markdown preview was clicked. */
  'open-file': [path: string, line: number | null]
  /** An http(s)/mailto link inside the markdown preview was clicked. */
  'open-url': [url: string]
}>()

const runsStore = useRunsStore()
const projectsStore = useProjectsStore()
const appSettings = useAppSettingsStore()

// ── File-type classification ──────────────────────────────────────────────
type FileKind = 'text' | 'image' | 'video' | 'audio' | 'pdf' | 'other'
const IMAGE_EXT = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico', 'avif', 'apng']
const VIDEO_EXT = ['mp4', 'webm', 'mov', 'm4v', 'ogv', 'mkv']
const AUDIO_EXT = ['mp3', 'wav', 'ogg', 'oga', 'flac', 'm4a', 'aac']
const OTHER_EXT = [
  'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'odp', 'rtf', 'pages', 'numbers', 'key',
  'zip', 'tar', 'gz', 'tgz', 'bz2', 'xz', 'rar', '7z',
  'exe', 'dmg', 'pkg', 'app', 'msi', 'deb', 'bin', 'so', 'dylib', 'dll', 'o', 'a', 'class', 'jar', 'wasm',
  'ttf', 'otf', 'woff', 'woff2', 'eot', 'psd', 'ai', 'sketch', 'fig', 'xcf', 'heic', 'tiff', 'tif', 'raw',
]
function extOf(path: string): string {
  const base = (path.split('/').pop() ?? path).toLowerCase()
  return base.includes('.') ? base.slice(base.lastIndexOf('.') + 1) : ''
}
function fileKind(path: string): FileKind {
  const ext = extOf(path)
  if (IMAGE_EXT.includes(ext)) return 'image'
  if (VIDEO_EXT.includes(ext)) return 'video'
  if (AUDIO_EXT.includes(ext)) return 'audio'
  if (ext === 'pdf') return 'pdf'
  if (OTHER_EXT.includes(ext)) return 'other'
  return 'text'
}

// ── Viewer state ──────────────────────────────────────────────────────────
const curPath = ref('')
const curLine = ref<number | null>(null)
const content = ref('')
const truncated = ref(false)
const loading = ref(false)
const error = ref<string | null>(null)
const copied = ref(false)
const pathCopied = ref(false)
const reloading = ref(false)
const kind = ref<FileKind>('text')
const assetUrl = ref('')
const absPath = ref('')
const bodyEl = ref<HTMLElement | null>(null)
// Scroll container wrapping both preview and raw views — selection detection for
// the "Dịch" translate trigger is scoped to this element.
const viewerBodyEl = ref<HTMLElement | null>(null)

const lines = computed(() => content.value.split('\n'))
const isMarkdown = computed(() => /\.(md|markdown|mdx)$/i.test(curPath.value))
const mode = ref<'code' | 'preview'>('code')

// ── Inline editing (text / markdown only) ───────────────────────────────────
// Only plain-text/markdown files that loaded fully (not truncated, no error)
// may be edited in place. Media, binaries and oversized files stay read-only.
const editing = ref(false)
const editContent = ref('')
const saving = ref(false)
const saveError = ref<string | null>(null)
const editable = computed(() =>
  kind.value === 'text' && !truncated.value && !error.value && !loading.value,
)
const dirty = computed(() => editing.value && editContent.value !== content.value)

function startEdit() {
  if (!editable.value) return
  editContent.value = content.value
  saveError.value = null
  editing.value = true
  // Markdown edits happen against the raw source, not the rendered preview.
  if (isMarkdown.value) mode.value = 'code'
}

function cancelEdit() {
  editing.value = false
  saveError.value = null
}

async function saveEdit() {
  if (saving.value || !editable.value) return
  saving.value = true
  saveError.value = null
  try {
    await runsStore.writeProjectFile(props.projectPath, curPath.value, editContent.value)
    content.value = editContent.value
    editing.value = false
  } catch (e) {
    saveError.value = String(e)
  } finally {
    saving.value = false
  }
}

// Adjustable font size (px) for text / markdown.
const fontSize = ref(13)
const FONT_MIN = 10
const FONT_MAX = 28
function bumpFontSize(delta: number) {
  fontSize.value = Math.max(FONT_MIN, Math.min(FONT_MAX, fontSize.value + delta))
}

// ── Markdown rendering (local instance, mirrors RunView) ────────────────────
const mdReady = ref(false)
let _md: MarkdownIt | null = null
let _sanitize: ((html: string) => string) | null = null
function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}
function renderText(md: string): string {
  if (!mdReady.value || !_md) return escapeHtml(md ?? '').replace(/\n/g, '<br/>')
  const html = _md.render(md ?? '')
  // html:true lets raw HTML through (needed for inline <svg> diagrams in docs),
  // so sanitize before v-html. DOMPurify's default config keeps SVG/MathML and
  // data-* attributes (so the mermaid placeholder survives) while stripping
  // <script>, event handlers and other XSS vectors — important because the app
  // runs with csp:null.
  return _sanitize ? _sanitize(html) : html
}
async function loadMarkdown() {
  if (_md) return
  try {
    const [MarkdownIt, DOMPurify] = await Promise.all([
      import('markdown-it').then(m => m.default),
      import('dompurify').then(m => m.default),
    ])
    _md = new MarkdownIt({ html: true, linkify: true, typographer: true, breaks: true })
    applyMermaidFence(_md)
    _sanitize = (html: string) => DOMPurify.sanitize(html, { USE_PROFILES: { html: true, svg: true, svgFilters: true } })
    mdReady.value = true
  } catch {
    /* leave _md null; renderText falls back to escaped text */
  }
}

// ── Project file index (for clickable file links in markdown preview) ───────
const projectFileSet = ref<Set<string>>(new Set())
async function loadProjectFiles() {
  if (!props.projectPath) return
  try {
    const entries = await runsStore.listProjectFiles(props.projectPath)
    projectFileSet.value = new Set(entries.filter(e => !e.is_dir).map(e => e.path))
  } catch {
    projectFileSet.value = new Set()
  }
}
const vFileLinks = {
  mounted: (el: HTMLElement) => decorateFileLinks(el, (raw) => matchProjectFile(raw, projectFileSet.value)),
  updated: (el: HTMLElement) => decorateFileLinks(el, (raw) => matchProjectFile(raw, projectFileSet.value)),
}
function onProseClick(e: MouseEvent) {
  const target = e.target as HTMLElement
  const code = target.closest('code.file-link') as HTMLElement | null
  const fp = code?.getAttribute('data-file-path')
  if (fp) { e.preventDefault(); emit('open-file', fp, null); return }
  const anchor = target.closest('a') as HTMLAnchorElement | null
  if (anchor) {
    e.preventDefault()
    const href = anchor.getAttribute('href') ?? ''
    if (!href) return
    if (/^(https?:|mailto:)/i.test(href)) { emit('open-url', href); return }
    const path = matchProjectFile(href.replace(/#.*$/, ''), projectFileSet.value)
    if (path) emit('open-file', path, parseLineRef(href))
  }
}

// ── Syntax highlighting (shiki) for the raw text view ───────────────────────
type HlToken = { content: string; color?: string }
const highlightedLines = ref<HlToken[][] | null>(null)
const MAX_HIGHLIGHT_BYTES = 200_000
const LANG_BY_EXT: Record<string, string> = {
  ts: 'typescript', tsx: 'tsx', mts: 'typescript', cts: 'typescript',
  js: 'javascript', jsx: 'jsx', mjs: 'javascript', cjs: 'javascript',
  vue: 'vue', svelte: 'svelte',
  rs: 'rust', go: 'go', py: 'python', rb: 'ruby', php: 'php',
  java: 'java', kt: 'kotlin', kts: 'kotlin', swift: 'swift', scala: 'scala',
  c: 'c', h: 'c', cpp: 'cpp', cc: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp',
  cs: 'csharp', m: 'objective-c', mm: 'objective-cpp',
  json: 'json', jsonc: 'jsonc', json5: 'json5',
  yaml: 'yaml', yml: 'yaml', toml: 'toml', ini: 'ini',
  xml: 'xml', html: 'html', htm: 'html',
  css: 'css', scss: 'scss', sass: 'sass', less: 'less',
  sh: 'bash', bash: 'bash', zsh: 'bash', fish: 'fish',
  sql: 'sql', graphql: 'graphql', gql: 'graphql',
  lua: 'lua', dart: 'dart', r: 'r', pl: 'perl',
  md: 'markdown', markdown: 'markdown', mdx: 'mdx',
  proto: 'proto', diff: 'diff', patch: 'diff',
}
function langFromPath(path: string): string | null {
  const base = (path.split('/').pop() ?? path).toLowerCase()
  if (base === 'dockerfile') return 'docker'
  if (base === 'makefile') return 'make'
  const ext = base.includes('.') ? base.slice(base.lastIndexOf('.') + 1) : ''
  return LANG_BY_EXT[ext] ?? null
}
let highlightSeq = 0
async function refreshHighlight() {
  highlightedLines.value = null
  if (mode.value !== 'code') return
  const code = content.value
  if (!code || code.length > MAX_HIGHLIGHT_BYTES) return
  const lang = langFromPath(curPath.value)
  if (!lang) return
  const seq = ++highlightSeq
  try {
    const { codeToTokens } = await import('shiki')
    const dark = document.documentElement.classList.contains('dark')
    const opts = { lang, theme: dark ? 'github-dark' : 'github-light' } as Parameters<typeof codeToTokens>[1]
    const { tokens } = await codeToTokens(code, opts)
    if (seq === highlightSeq) {
      highlightedLines.value = tokens.map((line) => line.map((t) => ({ content: t.content, color: t.color })))
    }
  } catch {
    /* unsupported language or load failure — fall back to plain text */
  }
}
watch([content, mode], refreshHighlight)

// ── Loading ─────────────────────────────────────────────────────────────────
function scrollToLine() {
  const line = curLine.value
  if (!line) return
  const el = bodyEl.value?.querySelector(`[data-line="${line}"]`) as HTMLElement | null
  el?.scrollIntoView({ block: 'center' })
}

async function load() {
  const projPath = props.projectPath
  const path = props.path
  if (!projPath || !path) return
  const abs = path.startsWith('/') ? path : `${projPath}/${path}`
  curPath.value = path
  curLine.value = props.line ?? null
  absPath.value = abs
  // Markdown opens rendered by default unless a specific line was requested
  // (the line-numbered raw view is what can scroll to it).
  mode.value = /\.(md|markdown|mdx)$/i.test(path) && !props.line ? 'preview' : 'code'
  error.value = null
  content.value = ''
  truncated.value = false
  copied.value = false
  assetUrl.value = ''
  editing.value = false
  saveError.value = null
  const k = fileKind(path)
  kind.value = k

  if (k !== 'text') {
    if (k !== 'other') assetUrl.value = convertFileSrc(abs)
    loading.value = false
    return
  }

  loading.value = true
  try {
    const res = await runsStore.readProjectFile(projPath, path)
    curPath.value = res.path
    truncated.value = res.truncated
    // Never render a partial view for oversized files — the "too large" notice
    // plus an external-editor button replaces the misleadingly incomplete text.
    content.value = res.truncated ? '' : res.content
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
  if (!error.value && curLine.value) {
    await nextTick()
    scrollToLine()
  }
}

function onOpenInApp() {
  if (absPath.value) openPath(absPath.value).catch(() => { /* opener unavailable */ })
}
function onRevealInFolder() {
  if (absPath.value) revealItemInDir(absPath.value).catch(() => { /* opener unavailable */ })
}
function onOpenInVscode() {
  if (absPath.value) projectsStore.openInVscode(absPath.value).catch(() => { /* VS Code unavailable */ })
}
async function copyPath() {
  const target = absPath.value || curPath.value
  if (!target) return
  try {
    await navigator.clipboard.writeText(target)
    pathCopied.value = true
    setTimeout(() => { pathCopied.value = false }, 1500)
  } catch { /* clipboard unavailable */ }
}
async function copyContent() {
  try {
    await navigator.clipboard.writeText(content.value)
    copied.value = true
    setTimeout(() => { copied.value = false }, 1500)
  } catch { /* clipboard unavailable */ }
}

// Re-read the file from disk, preserving the current view mode / font size /
// scroll position (unlike load(), which resets them on a fresh open).
async function reload() {
  const projPath = props.projectPath
  const path = props.path
  if (!projPath || !path || reloading.value) return
  if (kind.value !== 'text') {
    // Re-resolve the asset URL so updated media re-fetches from disk.
    if (kind.value !== 'other') assetUrl.value = convertFileSrc(absPath.value)
    return
  }
  reloading.value = true
  error.value = null
  editing.value = false
  saveError.value = null
  try {
    const res = await runsStore.readProjectFile(projPath, path)
    curPath.value = res.path
    truncated.value = res.truncated
    content.value = res.truncated ? '' : res.content
  } catch (e) {
    error.value = String(e)
  } finally {
    reloading.value = false
  }
}

// ── Translate selection ─────────────────────────────────────────────────────
// Drag-selecting text inside the file body (rendered markdown or raw source)
// surfaces a floating "Dịch" button; clicking it opens a TranslatePopover that
// calls the one-shot `translate_text` backend command. Mirrors RunView.
const translateTrigger = ref<{ text: string; x: number; y: number } | null>(null)
const activeTranslation = ref<{ text: string; x: number; y: number } | null>(null)
const defaultTranslateLang = computed(() => appSettings.settings?.translate_target_lang || 'vi')

function clearTranslateTrigger() {
  translateTrigger.value = null
}

function onSelectionMouseUp() {
  const sel = window.getSelection()
  if (!sel || sel.isCollapsed) { translateTrigger.value = null; return }
  const text = sel.toString().trim()
  if (text.length < 2) { translateTrigger.value = null; return }
  const container = viewerBodyEl.value
  if (!container || (!container.contains(sel.anchorNode) && !container.contains(sel.focusNode))) {
    translateTrigger.value = null
    return
  }
  let rect: DOMRect | null = null
  try { rect = sel.getRangeAt(0).getBoundingClientRect() } catch { rect = null }
  if (!rect || (rect.width === 0 && rect.height === 0)) { translateTrigger.value = null; return }
  translateTrigger.value = { text, x: rect.left, y: rect.bottom + 6 }
}

function openTranslate() {
  const t = translateTrigger.value
  if (!t) return
  activeTranslation.value = { ...t }
  translateTrigger.value = null
}

function closeTranslate() {
  activeTranslation.value = null
}

// A new file replaces the body content — drop any stale translate UI.
watch(() => [props.projectPath, props.path], () => {
  clearTranslateTrigger()
  closeTranslate()
})

onMounted(() => {
  loadMarkdown()
  loadProjectFiles()
  load()
  window.addEventListener('mouseup', onSelectionMouseUp)
})
onUnmounted(() => {
  window.removeEventListener('mouseup', onSelectionMouseUp)
})
watch(() => [props.projectPath, props.path, props.line], () => {
  loadProjectFiles()
  load()
})

defineExpose({ onRevealInFolder, onOpenInApp })
</script>

<template>
  <div class="flex flex-col h-full min-h-0 bg-card">
    <!-- Toolbar -->
    <div class="sticky top-0 z-10 flex items-center gap-2 border-b border-border bg-card px-4 py-3 shrink-0">
      <FileCode2 class="h-4 w-4 text-primary shrink-0" :stroke-width="1.75" />
      <span class="text-xs font-mono text-foreground/90 truncate flex-1" :title="curPath">{{ curPath }}</span>
      <!-- Raw / rendered toggle, only meaningful for markdown files -->
      <div
        v-if="isMarkdown && content && !editing"
        class="flex items-center rounded-md border border-border overflow-hidden shrink-0 text-[10px] font-medium"
      >
        <button
          class="px-2 py-1 transition-colors cursor-pointer"
          :class="mode === 'preview' ? 'bg-primary/15 text-primary' : 'text-foreground/50 hover:text-foreground'"
          @click="mode = 'preview'"
        >Preview</button>
        <button
          class="px-2 py-1 border-l border-border transition-colors cursor-pointer"
          :class="mode === 'code' ? 'bg-primary/15 text-primary' : 'text-foreground/50 hover:text-foreground'"
          @click="mode = 'code'"
        >Raw</button>
      </div>
      <!-- Font size controls -->
      <div v-if="content" class="flex items-center rounded-md border border-border overflow-hidden shrink-0">
        <button
          class="flex items-center justify-center h-6 w-6 text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-40 disabled:cursor-default"
          title="Smaller font"
          :disabled="fontSize <= FONT_MIN"
          @click="bumpFontSize(-1)"
        >
          <AArrowDown class="h-3.5 w-3.5" :stroke-width="1.75" />
        </button>
        <span class="px-1.5 text-[10px] tabular-nums text-foreground/50 border-x border-border select-none">{{ fontSize }}</span>
        <button
          class="flex items-center justify-center h-6 w-6 text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-40 disabled:cursor-default"
          title="Larger font"
          :disabled="fontSize >= FONT_MAX"
          @click="bumpFontSize(1)"
        >
          <AArrowUp class="h-3.5 w-3.5" :stroke-width="1.75" />
        </button>
      </div>
      <!-- Copy the open file's path to the clipboard -->
      <button
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        :title="pathCopied ? 'Path copied!' : 'Copy file path'"
        @click="copyPath"
      >
        <Check v-if="pathCopied" class="h-3.5 w-3.5 text-primary" :stroke-width="1.75" />
        <ClipboardCopy v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
      <!-- Open in the OS default app — most useful for media / office files -->
      <button
        v-if="kind !== 'text'"
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        title="Open in default app"
        @click="onOpenInApp"
      >
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
      <!-- Reload the file's latest content from disk -->
      <button
        v-if="kind === 'text'"
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0 disabled:opacity-40 disabled:cursor-default"
        title="Reload from disk"
        :disabled="reloading"
        @click="reload"
      >
        <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': reloading }" :stroke-width="1.75" />
      </button>
      <button
        v-if="content"
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        :title="copied ? 'Copied!' : 'Copy content'"
        @click="copyContent"
      >
        <Check v-if="copied" class="h-3.5 w-3.5 text-primary" :stroke-width="1.75" />
        <Copy v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
      <!-- Edit toggle (text / markdown files only) -->
      <button
        v-if="editable && !editing"
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        title="Edit file"
        @click="startEdit"
      >
        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
      <!-- Save / cancel controls while editing -->
      <template v-if="editing">
        <button
          class="flex items-center gap-1 h-6 px-2 rounded-md text-[11px] font-medium bg-primary/15 text-primary hover:bg-primary/25 transition-colors cursor-pointer shrink-0 disabled:opacity-40 disabled:cursor-default"
          title="Save changes"
          :disabled="saving || !dirty"
          @click="saveEdit"
        >
          <Save class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ saving ? 'Saving…' : 'Save' }}
        </button>
        <button
          class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
          title="Cancel editing"
          @click="cancelEdit"
        >
          <X class="h-3.5 w-3.5" :stroke-width="1.75" />
        </button>
      </template>
      <!-- Host-supplied chrome controls (full-screen, pop-out, close) -->
      <slot name="actions" />
    </div>

    <!-- Body -->
    <div ref="viewerBodyEl" class="flex-1 overflow-auto min-h-0" @scroll="clearTranslateTrigger">
      <div v-if="loading" class="p-4 text-xs text-muted-foreground">Loading…</div>
      <div v-else-if="error" class="p-6 flex flex-col items-center gap-3 text-center">
        <FileQuestion class="h-10 w-10 text-foreground/30" :stroke-width="1.5" />
        <div>
          <p class="text-sm font-medium text-destructive mb-1">Could not open file</p>
          <p class="text-xs text-foreground/50 font-mono break-all max-w-md">{{ error }}</p>
        </div>
        <div class="flex gap-2">
          <Button size="sm" @click="onOpenInApp">
            <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" /> Open in default app
          </Button>
          <Button size="sm" variant="outline" @click="onRevealInFolder">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" /> Reveal in folder
          </Button>
        </div>
      </div>
      <!-- Image -->
      <div v-else-if="kind === 'image'" class="flex items-center justify-center p-4 bg-foreground/5 min-h-[200px]">
        <img :src="assetUrl" :alt="curPath" class="max-w-full max-h-full object-contain" />
      </div>
      <!-- Video -->
      <div v-else-if="kind === 'video'" class="flex items-center justify-center bg-black">
        <video :src="assetUrl" controls class="max-w-full max-h-[85vh]" />
      </div>
      <!-- Audio -->
      <div v-else-if="kind === 'audio'" class="p-8 flex items-center justify-center">
        <audio :src="assetUrl" controls class="w-full max-w-lg" />
      </div>
      <!-- PDF -->
      <iframe
        v-else-if="kind === 'pdf'"
        :src="assetUrl"
        class="w-full h-full min-h-[400px] bg-white"
        title="PDF preview"
      />
      <!-- Non-previewable (office docs, archives, binaries) -->
      <div v-else-if="kind === 'other'" class="p-10 flex flex-col items-center gap-3 text-center">
        <FileQuestion class="h-12 w-12 text-foreground/30" :stroke-width="1.5" />
        <div>
          <p class="text-sm font-medium text-foreground/80 mb-1">Preview not available</p>
          <p class="text-xs text-foreground/50">This file type can't be shown here. Open it in its native app instead.</p>
        </div>
        <div class="flex gap-2">
          <Button size="sm" @click="onOpenInApp">
            <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" /> Open in default app
          </Button>
          <Button size="sm" variant="outline" @click="onRevealInFolder">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" /> Reveal in folder
          </Button>
        </div>
      </div>
      <!-- Oversized text file: refuse to render a partial view, point the user
           at an external editor that can open the whole thing. -->
      <div v-else-if="truncated" class="p-10 flex flex-col items-center gap-3 text-center">
        <FileWarning class="h-12 w-12 text-amber-500/70" :stroke-width="1.5" />
        <div>
          <p class="text-sm font-medium text-foreground/80 mb-1">File too large to preview</p>
          <p class="text-xs text-foreground/50 max-w-md">This file is larger than 2&nbsp;MB. Open it in an external editor to view the full contents.</p>
        </div>
        <div class="flex gap-2">
          <Button size="sm" @click="onOpenInVscode">
            <Code2 class="h-3.5 w-3.5" :stroke-width="1.75" /> Open in VS Code
          </Button>
          <Button size="sm" variant="outline" @click="onRevealInFolder">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" /> Reveal in folder
          </Button>
        </div>
      </div>
      <!-- Inline editor (text / markdown), plain textarea against raw source -->
      <div v-else-if="editing" class="flex flex-col h-full min-h-[70vh]">
        <textarea
          v-model="editContent"
          spellcheck="false"
          class="flex-1 w-full resize-none bg-card text-foreground/90 font-mono px-4 py-2 outline-none leading-relaxed min-h-[60vh]"
          :style="{ fontSize: fontSize + 'px' }"
        />
        <p
          v-if="saveError"
          class="shrink-0 px-4 py-2 text-xs text-destructive border-t border-border bg-destructive/10 break-all"
        >{{ saveError }}</p>
      </div>
      <!-- Rendered markdown preview -->
      <div
        v-else-if="isMarkdown && mode === 'preview'"
        v-file-links
        v-mermaid
        v-copy-code
        v-html="renderText(content)"
        class="markdown-output p-4 leading-relaxed text-foreground"
        :style="{ fontSize: fontSize + 'px' }"
        @click="onProseClick"
      />
      <!-- Raw, line-numbered source -->
      <div
        v-else
        ref="bodyEl"
        class="py-2 leading-relaxed font-mono text-foreground/90"
        :style="{ fontSize: fontSize + 'px' }"
      >
        <div
          v-for="(ln, idx) in lines"
          :key="idx"
          :data-line="idx + 1"
          class="flex min-w-max"
          :class="{ 'bg-primary/15': idx + 1 === curLine }"
        >
          <span
            class="sticky left-0 select-none text-right tabular-nums shrink-0 w-12 pr-3 pl-2 border-r border-border bg-card"
            :class="idx + 1 === curLine ? 'text-primary' : 'text-foreground/30'"
          >{{ idx + 1 }}</span>
          <span class="px-3 whitespace-pre flex-1">
            <template v-if="highlightedLines && highlightedLines[idx] && highlightedLines[idx].length">
              <span
                v-for="(t, ti) in highlightedLines[idx]"
                :key="ti"
                :style="t.color ? { color: t.color } : undefined"
              >{{ t.content }}</span>
            </template>
            <template v-else>{{ ln || ' ' }}</template>
          </span>
        </div>
      </div>
    </div>

    <!-- Floating "Dịch" trigger for a text selection in the file body.
         `mousedown.prevent` keeps the selection alive through the click. -->
    <Teleport to="body">
      <button
        v-if="translateTrigger"
        class="fixed z-[65] inline-flex items-center gap-1 rounded-md border border-border bg-card px-2 py-1 text-[11px] font-medium text-primary shadow-lg shadow-black/30 hover:bg-accent/60 transition-colors cursor-pointer"
        :style="{ left: translateTrigger.x + 'px', top: translateTrigger.y + 'px' }"
        title="Dịch đoạn đã chọn"
        @mousedown.prevent
        @click="openTranslate"
      >
        <Languages class="h-3 w-3" :stroke-width="1.75" />
        Dịch
      </button>
    </Teleport>

    <!-- Translation result popover -->
    <TranslatePopover
      v-if="activeTranslation"
      :text="activeTranslation.text"
      :x="activeTranslation.x"
      :y="activeTranslation.y"
      :initial-lang="defaultTranslateLang"
      @close="closeTranslate"
    />
  </div>
</template>
