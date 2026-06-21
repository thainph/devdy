<script setup lang="ts">
import { ref, computed, watch, type Component } from 'vue'
import {
  ChevronRight, Wrench, Sparkles, CheckCircle2, XCircle, Cpu, Brain, AlertTriangle, Info,
  Terminal, FilePen, FilePlus2, FileText, Search, Globe, ListTodo, User,
} from 'lucide-vue-next'
import type { StreamEntry } from '@/lib/streamEvents'

const props = defineProps<{
  entries: StreamEntry[]
  running?: boolean
  renderText: (md: string) => string
  /** Resolve a raw inline-code token to a project file path, or null. */
  fileMatcher?: (raw: string) => string | null
}>()

const emit = defineEmits<{
  (e: 'open-file', path: string, line?: number | null): void
  (e: 'open-url', url: string): void
}>()

// Pull a 1-based line number from a link target like `foo.ts#L635`,
// `foo.ts#L177-L189`, or `foo.ts:635`. Returns null when there's none.
function parseLineRef(ref: string): number | null {
  const m = ref.match(/#L?(\d+)/i) ?? ref.match(/:(\d+)(?:-\d+)?$/)
  return m ? parseInt(m[1], 10) : null
}

// Directive: mark inline `<code>` tokens that name a real project file so they
// render as clickable file links (handled by the container's click listener).
const vFileLinks = {
  mounted: (el: HTMLElement) => decorateFileLinks(el),
  updated: (el: HTMLElement) => decorateFileLinks(el),
}

function decorateFileLinks(el: HTMLElement) {
  const match = props.fileMatcher
  if (!match) return
  el.querySelectorAll('code').forEach((code) => {
    if (code.closest('pre')) return // skip multi-line code blocks
    const path = match((code.textContent ?? '').trim())
    if (path) {
      code.classList.add('file-link')
      code.setAttribute('data-file-path', path)
    } else {
      code.classList.remove('file-link')
      code.removeAttribute('data-file-path')
    }
  })
}

function onProseClick(e: MouseEvent) {
  const target = e.target as HTMLElement
  // Inline `<code>` that names a project file.
  const code = target.closest('code.file-link') as HTMLElement | null
  const codePath = code?.getAttribute('data-file-path')
  if (codePath) {
    e.preventDefault()
    emit('open-file', codePath)
    return
  }
  // Markdown link: never let the webview follow it (a relative href blanks the
  // whole SPA). Open project files in the viewer, external URLs in the browser.
  const anchor = target.closest('a') as HTMLAnchorElement | null
  if (anchor) {
    e.preventDefault()
    const href = anchor.getAttribute('href') ?? ''
    if (!href) return
    if (/^(https?:|mailto:)/i.test(href)) {
      emit('open-url', href)
      return
    }
    const path = props.fileMatcher?.(href.replace(/#.*$/, '')) ?? null
    if (path) emit('open-file', path, parseLineRef(href))
  }
}

// Tools whose input carries a file path we can open in the viewer.
const FILE_PATH_KEYS = ['file_path', 'notebook_path', 'path']

// Return the file path a tool acts on, or null when the tool isn't file-based.
function fileTarget(input: unknown): string | null {
  if (!input || typeof input !== 'object') return null
  const obj = input as Record<string, unknown>
  for (const k of FILE_PATH_KEYS) {
    const v = obj[k]
    if (typeof v === 'string' && v.trim()) return v
  }
  return null
}

// Per-tool visual identity: distinct icon + accent color per tool category,
// so exec/edit/write/read/search stand out and read apart from prose.
interface ToolStyle {
  icon: Component
  iconClass: string
  nameClass: string
  barClass: string // left accent border color
}

const DEFAULT_TOOL_STYLE: ToolStyle = {
  icon: Wrench,
  iconClass: 'text-indigo-400/80',
  nameClass: 'text-indigo-500 dark:text-indigo-300/90',
  barClass: 'border-l-indigo-500/50',
}

function toolStyle(name: string): ToolStyle {
  const n = (name || '').toLowerCase()
  if (n === 'bash' || n === 'bashoutput' || n === 'killshell' || n === 'killbash')
    return { icon: Terminal, iconClass: 'text-emerald-400', nameClass: 'text-emerald-600 dark:text-emerald-300', barClass: 'border-l-emerald-500/60' }
  if (n === 'edit' || n === 'multiedit' || n === 'notebookedit')
    return { icon: FilePen, iconClass: 'text-amber-400', nameClass: 'text-amber-600 dark:text-amber-300', barClass: 'border-l-amber-500/60' }
  if (n === 'write')
    return { icon: FilePlus2, iconClass: 'text-rose-400', nameClass: 'text-rose-600 dark:text-rose-300', barClass: 'border-l-rose-500/60' }
  if (n === 'read')
    return { icon: FileText, iconClass: 'text-sky-400', nameClass: 'text-sky-600 dark:text-sky-300', barClass: 'border-l-sky-500/60' }
  if (n === 'grep' || n === 'glob')
    return { icon: Search, iconClass: 'text-violet-400', nameClass: 'text-violet-600 dark:text-violet-300', barClass: 'border-l-violet-500/60' }
  if (n === 'webfetch' || n === 'websearch')
    return { icon: Globe, iconClass: 'text-cyan-400', nameClass: 'text-cyan-600 dark:text-cyan-300', barClass: 'border-l-cyan-500/60' }
  if (n === 'todowrite')
    return { icon: ListTodo, iconClass: 'text-blue-400', nameClass: 'text-blue-600 dark:text-blue-300', barClass: 'border-l-blue-500/60' }
  return DEFAULT_TOOL_STYLE
}

const expanded = ref<Record<number, boolean>>({})

watch(
  () => props.entries.length,
  () => {
    for (let i = 0; i < props.entries.length; i++) {
      const e = props.entries[i]
      if (e.kind === 'tool' && expanded.value[i] === undefined) {
        expanded.value[i] = !!e.result?.is_error
      }
    }
  },
  { immediate: true }
)

function toggle(i: number) {
  expanded.value[i] = !expanded.value[i]
}

function previewInput(input: unknown): string {
  if (input == null) return ''
  if (typeof input === 'string') return input
  if (typeof input === 'object') {
    const obj = input as Record<string, unknown>
    const preferred = ['file_path', 'path', 'command', 'pattern', 'query', 'url']
    for (const k of preferred) {
      if (typeof obj[k] === 'string') return obj[k] as string
    }
    const json = JSON.stringify(obj)
    return json.length > 80 ? json.slice(0, 80) + '…' : json
  }
  return String(input)
}

function prettyInput(input: unknown): string {
  if (input == null) return ''
  if (typeof input === 'string') return input
  try {
    return JSON.stringify(input, null, 2)
  } catch {
    return String(input)
  }
}

// Muted badge colors per diagnostic level — calm, not alarming.
function logBadgeClass(level: string): string {
  switch (level) {
    case 'warn': return 'text-amber-600 dark:text-amber-400/90 bg-amber-500/10'
    case 'error': return 'text-red-600 dark:text-red-400/90 bg-red-500/10'
    case 'debug':
    case 'trace': return 'text-foreground/40 bg-foreground/5'
    default: return 'text-sky-600 dark:text-sky-400/90 bg-sky-500/10'
  }
}

function formatDuration(ms?: number): string {
  if (!ms) return ''
  if (ms < 1000) return `${ms}ms`
  return `${(ms / 1000).toFixed(1)}s`
}

function formatTokens(n?: number): string {
  if (!n) return '0'
  if (n < 1000) return String(n)
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`
  return `${(n / 1_000_000).toFixed(2)}M`
}

const lastEntryIsResult = computed(() => {
  const last = props.entries[props.entries.length - 1]
  return last?.kind === 'result'
})
</script>

<template>
  <div class="space-y-3">
    <template v-for="(entry, i) in entries" :key="i">
      <!-- System init banner -->
      <div
        v-if="entry.kind === 'system'"
        class="flex items-center gap-2 text-[10px] font-mono text-foreground/40 border-b border-border pb-2"
      >
        <Cpu class="h-3 w-3" :stroke-width="1.5" />
        <span>session</span>
        <span v-if="entry.model" class="text-foreground/70">{{ entry.model }}</span>
        <span v-if="entry.tools?.length" class="text-foreground/30">· {{ entry.tools.length }} tools</span>
      </div>

      <!-- User follow-up — right-aligned chat bubble with avatar -->
      <div v-else-if="entry.kind === 'user'" class="flex justify-end items-end gap-2 pl-10 my-1">
        <div
          class="max-w-[82%] rounded-2xl rounded-br-md bg-primary/15 border border-primary/25 px-3.5 py-2 text-sm leading-relaxed text-foreground/90 shadow-sm"
        >
          <div
            v-if="entry.images && entry.images.length"
            class="flex flex-wrap gap-1.5"
            :class="entry.text ? 'mb-2' : ''"
          >
            <img
              v-for="(img, ii) in entry.images"
              :key="ii"
              :src="`data:${img.media_type};base64,${img.data}`"
              class="max-h-40 max-w-48 rounded-md border border-primary/20 object-contain"
              alt="attachment"
            />
          </div>
          <div v-if="entry.text" class="whitespace-pre-wrap">{{ entry.text }}</div>
        </div>
        <div
          class="shrink-0 h-7 w-7 rounded-full bg-primary/20 border border-primary/30 flex items-center justify-center"
          title="You"
        >
          <User class="h-3.5 w-3.5 text-primary" :stroke-width="2" />
        </div>
      </div>

      <!-- Assistant text -->
      <div
        v-else-if="entry.kind === 'text'"
        v-file-links
        v-html="renderText(entry.text)"
        class="markdown-output text-sm leading-relaxed text-foreground"
        @click="onProseClick"
      />

      <!-- Thinking -->
      <div
        v-else-if="entry.kind === 'thinking'"
        class="text-xs italic text-foreground/40 border-l-2 border-foreground/10 pl-3 py-1"
      >
        <div class="flex items-center gap-1 mb-1 text-[10px] uppercase tracking-wider not-italic">
          <Brain class="h-3 w-3" :stroke-width="1.5" />
          thinking
        </div>
        <div class="whitespace-pre-wrap">{{ entry.text }}</div>
      </div>

      <!-- Tool call -->
      <div
        v-else-if="entry.kind === 'tool'"
        class="rounded-md border border-l-[3px] bg-foreground/2 overflow-hidden"
        :class="entry.result?.is_error
          ? 'border-border border-l-red-500/70'
          : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button
          class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/4 transition-colors cursor-pointer"
          @click="toggle(i)"
        >
          <ChevronRight
            class="h-3 w-3 text-foreground/40 transition-transform shrink-0"
            :stroke-width="1.75"
            :class="{ 'rotate-90': expanded[i] }"
          />
          <component
            :is="toolStyle(entry.name).icon"
            class="h-3.5 w-3.5 shrink-0"
            :class="toolStyle(entry.name).iconClass"
            :stroke-width="1.75"
          />
          <span class="text-xs font-mono font-semibold shrink-0" :class="toolStyle(entry.name).nameClass">{{ entry.name }}</span>
          <!-- File-based tool: the path is clickable to preview the file. -->
          <span
            v-if="fileTarget(entry.input)"
            class="text-[11px] font-mono text-sky-600 dark:text-sky-400/90 truncate underline decoration-dotted underline-offset-2 hover:text-sky-500 cursor-pointer"
            :title="'Open ' + fileTarget(entry.input)"
            role="button"
            @click.stop="emit('open-file', fileTarget(entry.input)!)"
          >{{ fileTarget(entry.input) }}</span>
          <span v-else class="text-[11px] font-mono text-foreground/40 truncate">{{ previewInput(entry.input) }}</span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5">
            <XCircle class="h-3 w-3" :stroke-width="1.75" />error
          </span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5">
            <CheckCircle2 class="h-3 w-3" :stroke-width="1.75" />
          </span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">running…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border">
          <div class="px-3 py-2 bg-foreground/4">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">input</div>
            <pre class="text-[11px] font-mono text-foreground/70 whitespace-pre-wrap break-words">{{ prettyInput(entry.input) }}</pre>
          </div>
          <div v-if="entry.result" class="px-3 py-2 border-t border-border">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">
              {{ entry.result.is_error ? 'error' : 'output' }}
            </div>
            <pre
              class="text-[11px] font-mono whitespace-pre-wrap break-words max-h-96 overflow-auto"
              :class="entry.result.is_error ? 'text-red-600 dark:text-red-300/90' : 'text-foreground/70'"
            >{{ entry.result.content }}</pre>
          </div>
        </div>
      </div>

      <!-- Result summary -->
      <div
        v-else-if="entry.kind === 'result'"
        class="mt-2 rounded-md border p-3"
        :class="entry.is_error
          ? 'border-red-500/30 bg-red-500/5'
          : 'border-emerald-500/20 bg-emerald-500/5'"
      >
        <div class="flex items-center gap-2 mb-2">
          <component
            :is="entry.is_error ? XCircle : CheckCircle2"
            class="h-4 w-4"
            :class="entry.is_error ? 'text-red-400' : 'text-emerald-400'"
            :stroke-width="2"
          />
          <span class="text-xs font-medium" :class="entry.is_error ? 'text-red-300' : 'text-emerald-300'">
            {{ entry.is_error ? 'Failed' : 'Completed' }}
          </span>
          <div class="ml-auto flex items-center gap-3 text-[10px] font-mono text-foreground/40">
            <span v-if="entry.duration_ms">{{ formatDuration(entry.duration_ms) }}</span>
            <span v-if="entry.num_turns">{{ entry.num_turns }} turns</span>
            <span v-if="entry.total_tokens">{{ formatTokens(entry.total_tokens) }} tok</span>
            <span v-if="entry.cost_usd" :title="entry.cost_estimated ? 'Estimated (subscription run)' : undefined">{{ entry.cost_estimated ? '~' : '' }}${{ entry.cost_usd.toFixed(4) }}</span>
          </div>
        </div>
        <div
          v-if="entry.text"
          v-file-links
          v-html="renderText(entry.text)"
          class="markdown-output text-sm leading-relaxed text-foreground"
          @click="onProseClick"
        />
      </div>

      <!-- diagnostic log line (codex tracing / sidecar notes) -->
      <div
        v-else-if="entry.kind === 'log'"
        class="flex items-start gap-2 text-[11px] font-mono text-foreground/45 leading-relaxed"
      >
        <Info class="h-3 w-3 mt-0.75 shrink-0 opacity-50" :stroke-width="1.75" />
        <span
          class="shrink-0 uppercase text-[9px] font-semibold tracking-wide px-1 py-px rounded mt-px"
          :class="logBadgeClass(entry.level)"
        >{{ entry.level }}</span>
        <span class="whitespace-pre-wrap break-all">{{ entry.text }}</span>
      </div>

      <!-- stderr line -->
      <div
        v-else-if="entry.kind === 'error'"
        class="flex items-start gap-2 text-xs font-mono text-amber-600 dark:text-amber-400/90"
      >
        <AlertTriangle class="h-3 w-3 mt-0.5 shrink-0" :stroke-width="1.75" />
        <span class="whitespace-pre-wrap">{{ entry.text }}</span>
      </div>
    </template>

    <!-- Running indicator -->
    <div
      v-if="running && !lastEntryIsResult"
      class="flex items-center gap-2 text-xs text-foreground/40 font-mono"
    >
      <Sparkles class="h-3 w-3 text-emerald-400 animate-pulse" :stroke-width="1.75" />
      <span class="animate-pulse">thinking…</span>
    </div>
  </div>
</template>
