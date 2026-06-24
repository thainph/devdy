<script setup lang="ts">
// Self-contained file viewer: loads a project file and renders it (syntax-
// highlighted text, rendered/raw markdown, images, video, audio, PDF, or an
// "open externally" fallback). Used both inside the in-app modal (RunView) and
// in the standalone pop-out window (FileViewerWindow). Hosts supply chrome-
// specific buttons (full-screen, pop-out, close) via the #actions slot.
import { ref, computed, watch, onMounted, nextTick } from 'vue'
import {
  FileCode2, AArrowDown, AArrowUp, ExternalLink, Copy, FileQuestion, FolderOpen,
} from 'lucide-vue-next'
import { convertFileSrc } from '@tauri-apps/api/core'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import type MarkdownIt from 'markdown-it'
import { Button } from '@/components/ui'
import { useRunsStore } from '@/stores/runs'
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
const kind = ref<FileKind>('text')
const assetUrl = ref('')
const absPath = ref('')
const bodyEl = ref<HTMLElement | null>(null)

const lines = computed(() => content.value.split('\n'))
const isMarkdown = computed(() => /\.(md|markdown|mdx)$/i.test(curPath.value))
const mode = ref<'code' | 'preview'>('code')

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
function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}
function renderText(md: string): string {
  if (!mdReady.value || !_md) return escapeHtml(md ?? '').replace(/\n/g, '<br/>')
  return _md.render(md ?? '')
}
async function loadMarkdown() {
  if (_md) return
  try {
    const MarkdownIt = (await import('markdown-it')).default
    _md = new MarkdownIt({ html: false, linkify: true, typographer: true, breaks: true })
    applyMermaidFence(_md)
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
    content.value = res.content
    truncated.value = res.truncated
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
async function copyContent() {
  try {
    await navigator.clipboard.writeText(content.value)
    copied.value = true
    setTimeout(() => { copied.value = false }, 1500)
  } catch { /* clipboard unavailable */ }
}

onMounted(() => {
  loadMarkdown()
  loadProjectFiles()
  load()
})
watch(() => [props.projectPath, props.path, props.line], () => {
  loadProjectFiles()
  load()
})

defineExpose({ onRevealInFolder, onOpenInApp })
</script>

<template>
  <div class="flex flex-col h-full min-h-0">
    <!-- Toolbar -->
    <div class="flex items-center gap-2 border-b border-border px-4 py-3 shrink-0">
      <FileCode2 class="h-4 w-4 text-primary shrink-0" :stroke-width="1.75" />
      <span class="text-xs font-mono text-foreground/90 truncate flex-1" :title="curPath">{{ curPath }}</span>
      <span v-if="truncated" class="text-[10px] text-amber-500 shrink-0">truncated</span>
      <!-- Raw / rendered toggle, only meaningful for markdown files -->
      <div
        v-if="isMarkdown && content"
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
      <!-- Open in the OS default app — most useful for media / office files -->
      <button
        v-if="kind !== 'text'"
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        title="Open in default app"
        @click="onOpenInApp"
      >
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
      <Button
        v-if="content"
        variant="outline"
        size="xs"
        class="shrink-0"
        :title="copied ? 'Copied!' : 'Copy content'"
        @click="copyContent"
      >
        <Copy class="h-3 w-3" :stroke-width="1.75" />
        {{ copied ? 'Copied' : 'Copy' }}
      </Button>
      <!-- Host-supplied chrome controls (full-screen, pop-out, close) -->
      <slot name="actions" />
    </div>

    <!-- Body -->
    <div class="flex-1 overflow-auto min-h-0">
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
  </div>
</template>
