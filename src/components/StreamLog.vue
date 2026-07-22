<script setup lang="ts">
import { ref, computed, watch, type Component } from 'vue'
import {
  ChevronRight, Wrench, Sparkles, CheckCircle2, XCircle, Cpu, Brain, AlertTriangle, Info,
  Terminal, FilePen, FilePlus2, FileText, Search, Globe, ListTodo, User, Archive, Copy, Check,
  Bot, MessageCircleQuestion, Circle, CircleDot, ClipboardList, Server,
} from 'lucide-vue-next'
import type { StreamEntry } from '@/lib/streamEvents'
import { vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { computeDiff } from '@/lib/diff'
import IdeContextChip from './IdeContextChip.vue'
import DiffView from './DiffView.vue'
import { CollapsibleMessage } from '@/components/ui'

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

// Per-entry expand state for compaction-summary dividers (collapsed by default).
const expandedCompacts = ref(new Set<number>())
function toggleCompact(i: number) {
  const next = new Set(expandedCompacts.value)
  next.has(i) ? next.delete(i) : next.add(i)
  expandedCompacts.value = next
}

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
  if (n === 'mcp' || n.startsWith('mcp__'))
    return { icon: Server, iconClass: 'text-teal-400', nameClass: 'text-teal-600 dark:text-teal-300', barClass: 'border-l-teal-500/60' }
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
  if (n === 'task' || n === 'agent')
    return { icon: Bot, iconClass: 'text-fuchsia-400', nameClass: 'text-fuchsia-600 dark:text-fuchsia-300', barClass: 'border-l-fuchsia-500/60' }
  if (n === 'askuserquestion')
    return { icon: MessageCircleQuestion, iconClass: 'text-blue-400', nameClass: 'text-blue-600 dark:text-blue-300', barClass: 'border-l-blue-500/60' }
  if (n === 'toolsearch')
    return { icon: Search, iconClass: 'text-teal-400', nameClass: 'text-teal-600 dark:text-teal-300', barClass: 'border-l-teal-500/60' }
  if (n === 'exitplanmode')
    return { icon: ClipboardList, iconClass: 'text-indigo-400', nameClass: 'text-indigo-600 dark:text-indigo-300', barClass: 'border-l-indigo-500/60' }
  return DEFAULT_TOOL_STYLE
}

// Bash-family tools get a dedicated terminal-style renderer (command + output),
// instead of the generic raw-JSON input dump used for other tools.
const BASH_TOOLS = new Set(['bash', 'bashoutput', 'killshell', 'killbash'])
function isBash(name: string): boolean {
  return BASH_TOOLS.has((name || '').toLowerCase())
}

interface BashInfo {
  command: string
  description: string
  timeout?: number
  background: boolean
}

// Normalize a Bash tool `input` (Claude sends an object with command/description;
// Codex sends `{ command }` where command may itself be an object) into the
// fields the terminal card renders. Memoized per input object: the template
// reads bashInfo several times per entry and the entries array is stable.
const bashInfoCache = new WeakMap<object, BashInfo>()
function bashInfo(input: unknown): BashInfo {
  if (input && typeof input === 'object') {
    const hit = bashInfoCache.get(input)
    if (hit) return hit
    const info = computeBashInfo(input)
    bashInfoCache.set(input, info)
    return info
  }
  return computeBashInfo(input)
}
function computeBashInfo(input: unknown): BashInfo {
  if (typeof input === 'string') return { command: input, description: '', background: false }
  if (!input || typeof input !== 'object') return { command: String(input ?? ''), description: '', background: false }
  const obj = input as Record<string, unknown>
  const rawCmd = obj.command ?? obj.cmd ?? obj.shell_command ?? ''
  const command = typeof rawCmd === 'string' ? rawCmd : JSON.stringify(rawCmd, null, 2)
  const description = typeof obj.description === 'string' ? obj.description : ''
  const timeout = typeof obj.timeout === 'number' ? obj.timeout : undefined
  const background = obj.run_in_background === true
  return { command, description, timeout, background }
}

// The header's primary line: purpose (description) when available, else the
// command itself. Shell-management tools (BashOutput / KillShell / KillBash)
// carry no command — they reference a background shell by id — so fall back to
// a readable label instead of showing a blank line.
function bashHeadline(name: string, input: unknown): string {
  const { description, command } = bashInfo(input)
  if (description) return description
  if (command) return command
  const o = asObj(input)
  const shellRef = asStr(o.bash_id) || asStr(o.shell_id)
  const n = (name || '').toLowerCase()
  if (n === 'bashoutput') return shellRef ? `Read output · ${shellRef}` : 'Read shell output'
  if (n === 'killshell' || n === 'killbash') return shellRef ? `Kill shell · ${shellRef}` : 'Kill shell'
  return name || 'command'
}

// Per-entry "copied" flash for the command copy button.
const copiedCmd = ref<Record<number, boolean>>({})
async function copyCommand(i: number, input: unknown) {
  try {
    await navigator.clipboard.writeText(bashInfo(input).command)
    copiedCmd.value[i] = true
    setTimeout(() => { copiedCmd.value[i] = false }, 1500)
  } catch {
    // Clipboard denied — ignore silently.
  }
}

// File-editing tools render as a unified diff instead of a raw-JSON dump.
const EDIT_TOOLS = new Set(['edit', 'multiedit', 'write'])
function isEditTool(name: string): boolean {
  return EDIT_TOOLS.has((name || '').toLowerCase())
}

interface DiffPart {
  before: string
  after: string
}

// Extract the before/after pair(s) a file-editing tool changes: Edit → one pair,
// MultiEdit → one per edit, Write → whole content as an all-added block.
function editParts(name: string, input: unknown): DiffPart[] {
  if (!input || typeof input !== 'object') return []
  const obj = input as Record<string, unknown>
  const n = (name || '').toLowerCase()
  const str = (v: unknown): string => (typeof v === 'string' ? v : '')
  if (n === 'edit') {
    const before = str(obj.old_string)
    const after = str(obj.new_string)
    return before || after ? [{ before, after }] : []
  }
  if (n === 'multiedit' && Array.isArray(obj.edits)) {
    return (obj.edits as unknown[])
      .map((e) => (e && typeof e === 'object' ? (e as Record<string, unknown>) : {}))
      .map((e) => ({ before: str(e.old_string), after: str(e.new_string) }))
      .filter((d) => d.before || d.after)
  }
  if (n === 'write') {
    const after = str(obj.content)
    return after ? [{ before: '', after }] : []
  }
  return []
}

// Aggregate +added / −removed across all diff parts for the header badge.
// Memoized per input object: the template reads editStats up to four times per
// entry and each call runs an O(n·m) LCS — computing it once per entry matters
// for MultiEdit calls with many parts.
const editStatsCache = new WeakMap<object, { added: number; removed: number }>()
function editStats(name: string, input: unknown): { added: number; removed: number } {
  if (!input || typeof input !== 'object') return { added: 0, removed: 0 }
  const hit = editStatsCache.get(input)
  if (hit) return hit
  const stats = editParts(name, input).reduce(
    (acc, p) => {
      const d = computeDiff(p.before, p.after, { fold: false })
      return { added: acc.added + d.added, removed: acc.removed + d.removed }
    },
    { added: 0, removed: 0 },
  )
  editStatsCache.set(input, stats)
  return stats
}

// Small helpers for structured tool inputs below.
function asObj(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}
function asStr(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

// --- Agent (Task): a delegated sub-agent run --------------------------------
function isAgentTool(name: string): boolean {
  const n = (name || '').toLowerCase()
  return n === 'task' || n === 'agent'
}
function agentInfo(input: unknown): { subagent: string; description: string; prompt: string } {
  const o = asObj(input)
  return { subagent: asStr(o.subagent_type), description: asStr(o.description), prompt: asStr(o.prompt) }
}

// --- AskUserQuestion --------------------------------------------------------
interface AskOption { label: string; description: string }
interface AskQuestion { question: string; header: string; multiSelect: boolean; options: AskOption[] }
function isAskUserTool(name: string): boolean {
  return (name || '').toLowerCase() === 'askuserquestion'
}
function askQuestions(input: unknown): AskQuestion[] {
  const raw = asObj(input).questions
  if (!Array.isArray(raw)) return []
  return raw.map((q) => {
    const qo = asObj(q)
    const options = Array.isArray(qo.options)
      ? (qo.options as unknown[]).map((x) => ({ label: asStr(asObj(x).label), description: asStr(asObj(x).description) })).filter((x) => x.label)
      : []
    return { question: asStr(qo.question), header: asStr(qo.header), multiSelect: qo.multiSelect === true, options }
  })
}

// --- ToolSearch -------------------------------------------------------------
function isToolSearchTool(name: string): boolean {
  return (name || '').toLowerCase() === 'toolsearch'
}
function toolSearchQuery(input: unknown): string {
  return asStr(asObj(input).query)
}

// --- TodoWrite --------------------------------------------------------------
interface TodoItem { content: string; status: string }
function isTodoTool(name: string): boolean {
  return (name || '').toLowerCase() === 'todowrite'
}
function todoItems(input: unknown): TodoItem[] {
  const raw = asObj(input).todos
  if (!Array.isArray(raw)) return []
  return (raw as unknown[]).map((t) => ({ content: asStr(asObj(t).content), status: asStr(asObj(t).status) || 'pending' }))
}
function todoStats(input: unknown): { done: number; total: number } {
  const items = todoItems(input)
  return { done: items.filter((t) => t.status === 'completed').length, total: items.length }
}

// --- Web (fetch/search): render the fetched content as markdown -------------
function isWebTool(name: string): boolean {
  const n = (name || '').toLowerCase()
  return n === 'webfetch' || n === 'websearch'
}

// --- MCP --------------------------------------------------------------------
function isMcpTool(name: string): boolean {
  const n = (name || '').toLowerCase()
  return n === 'mcp' || n.startsWith('mcp__')
}
function titleCaseWords(raw: string): string {
  return raw
    .replace(/[_-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .replace(/\b\w/g, (m) => m.toUpperCase())
}
function mcpRawParts(name: string): { server: string; action: string } {
  const parts = (name || '').split('__')
  if (parts.length >= 3 && parts[0].toLowerCase() === 'mcp') {
    return { server: parts[1], action: parts.slice(2).join('__') }
  }
  return { server: '', action: '' }
}
function mcpServerLabel(entry: Extract<StreamEntry, { kind: 'tool' }>): string {
  const inputServer = asStr(asObj(entry.input).serverName)
  return entry.serverDisplayName || inputServer || mcpRawParts(entry.name).server || 'MCP'
}
function mcpActionLabel(entry: Extract<StreamEntry, { kind: 'tool' }>): string {
  if (entry.displayName) return entry.displayName
  const inputTool = asStr(asObj(entry.input).toolName)
  const rawAction = inputTool || mcpRawParts(entry.name).action
  return rawAction ? titleCaseWords(rawAction) : 'MCP request'
}
function mcpSummary(entry: Extract<StreamEntry, { kind: 'tool' }>): string {
  const input = asObj(entry.input)
  const message = asStr(input.message)
  if (message) return message
  const questions = input.questions
  if (Array.isArray(questions)) {
    const first = asStr(asObj(questions[0]).question)
    if (first) return first
  }
  return asStr(input.mode) || previewInput(entry.input)
}

// --- ExitPlanMode: Claude presenting a plan to leave plan mode --------------
// Render the proposed plan as markdown, not a raw tool-input dump.
function isPlanTool(name: string): boolean {
  return (name || '').toLowerCase() === 'exitplanmode'
}
function planContent(input: unknown): string {
  return asStr(asObj(input).plan)
}

// The two most recent messages (assistant replies or user turns) are what the
// user is actively reading — and while running the last one is still streaming
// — so they always render in full, never collapsed.
const expandedMessageIndices = computed(() => {
  const indices = new Set<number>()
  for (let i = props.entries.length - 1; i >= 0 && indices.size < 2; i--) {
    const k = props.entries[i].kind
    if (k === 'text' || k === 'user') indices.add(i)
  }
  return indices
})

const expanded = ref<Record<number, boolean>>({})

watch(
  () => props.entries.length,
  () => {
    for (let i = 0; i < props.entries.length; i++) {
      const e = props.entries[i]
      if (e.kind === 'tool' && expanded.value[i] === undefined) {
        // Plans are meant to be read — show them open; other tools start collapsed
        // unless they errored.
        expanded.value[i] = isPlanTool(e.name) || !!e.result?.is_error
      }
    }
  },
  { immediate: true }
)

function toggle(i: number) {
  expanded.value[i] = !expanded.value[i]
}

// A run of user text: either plain prose or an `@file` mention that resolves to
// a real project file (and is therefore clickable to open in the viewer).
interface TextSegment {
  text: string
  path?: string
}

// Clickable file token in a user message, matched as either:
//   1. A backticked absolute path — `/Users/.../file.md` — produced by the
//      drag-drop composer's file-attachment block. Always clickable.
//   2. An `@`-mention — `@src/foo.ts` — clickable only when fileMatcher resolves
//      it to an actual project file.
const FILE_TOKEN_RE = /`(\/[^`\n]+)`|@(\S+)/g

// Split a user message into plain + clickable segments. Backticked absolute
// paths (dropped-file attachments) become links regardless of the project;
// `@tokens` become links only when they resolve to a project file. Everything
// else (emails, `@` in prose, unknown paths) stays plain text.
function userSegments(text: string): TextSegment[] {
  const match = props.fileMatcher
  const segs: TextSegment[] = []
  let last = 0
  FILE_TOKEN_RE.lastIndex = 0
  let m: RegExpExecArray | null
  while ((m = FILE_TOKEN_RE.exec(text))) {
    // Branch 1: backticked absolute path — attachment reference, always linked.
    if (m[1] !== undefined) {
      if (m.index > last) segs.push({ text: text.slice(last, m.index) })
      segs.push({ text: m[1], path: m[1] })
      last = m.index + m[0].length
      continue
    }
    // Branch 2: `@`-mention — link only when it resolves to a project file.
    const raw = m[2]
    if (!match) continue
    // Keep trailing punctuation (`,` `.` `)`…) outside the link, matching how
    // fileMatcher itself trims it when resolving the path.
    const core = raw.replace(/[)\].,:;'"`]+$/, '')
    const path = match(core)
    if (!path) continue
    if (m.index > last) segs.push({ text: text.slice(last, m.index) })
    segs.push({ text: '@' + core, path })
    const trailing = raw.slice(core.length)
    if (trailing) segs.push({ text: trailing })
    last = m.index + m[0].length
  }
  if (last < text.length) segs.push({ text: text.slice(last) })
  return segs.length ? segs : [{ text }]
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
          class="max-w-[82%] rounded-2xl rounded-br-md bg-muted/70 border border-border px-3.5 py-2 text-sm leading-relaxed text-foreground shadow-sm"
        >
          <div
            v-if="entry.ideContext?.length"
            class="mb-2 pb-2 border-b border-border flex flex-col items-end"
          >
            <IdeContextChip :items="entry.ideContext" variant="inside" @open-file="(p) => emit('open-file', p)" />
          </div>
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
          <CollapsibleMessage v-if="entry.text" :max-height="256" fade="hsl(var(--muted))" :disabled="expandedMessageIndices.has(i)">
            <div class="whitespace-pre-wrap"><template
                v-for="(seg, si) in userSegments(entry.text)"
                :key="si"
              ><span
                  v-if="seg.path"
                  class="font-medium text-primary cursor-pointer underline decoration-dotted underline-offset-2 hover:text-primary/75"
                  :title="'Open ' + seg.path"
                  role="button"
                  @click="emit('open-file', seg.path!)"
                >{{ seg.text }}</span><template v-else>{{ seg.text }}</template></template></div>
          </CollapsibleMessage>
        </div>
        <div
          class="shrink-0 h-7 w-7 rounded-full bg-muted border border-border flex items-center justify-center"
          title="You"
        >
          <User class="h-3.5 w-3.5 text-primary/70" :stroke-width="2" />
        </div>
      </div>

      <!-- IDE context — file the editor auto-attached to the turn (opened file
           or selection). A subtle chip, not a chat bubble: the user didn't type
           it. The filename opens the file in the viewer. -->
      <div v-else-if="entry.kind === 'ide-context'" class="flex justify-end pl-10 -my-0.5">
        <IdeContextChip :items="entry.items" variant="standalone" @open-file="(p) => emit('open-file', p)" />
      </div>

      <!-- Compaction summary — context was condensed; show a divider, expandable to read it -->
      <div v-else-if="entry.kind === 'compact'" class="my-3">
        <div class="flex items-center gap-2 text-foreground/40">
          <span class="h-px flex-1 bg-border" />
          <button
            class="inline-flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide hover:text-foreground/70 transition-colors"
            :title="expandedCompacts.has(i) ? 'Hide summary' : 'Show summary'"
            @click="toggleCompact(i)"
          >
            <Archive class="h-3 w-3" :stroke-width="1.75" />
            Context compacted
            <ChevronRight class="h-3 w-3 transition-transform" :class="expandedCompacts.has(i) ? 'rotate-90' : ''" :stroke-width="2" />
          </button>
          <span class="h-px flex-1 bg-border" />
        </div>
        <div
          v-if="expandedCompacts.has(i)"
          v-file-links
          v-mermaid
          v-copy-code
          v-html="renderText(entry.summary)"
          class="markdown-output mt-2 text-xs leading-relaxed text-foreground/70 bg-muted/40 border border-border rounded-lg px-3 py-2.5"
          @click="onProseClick"
        />
      </div>

      <!-- Local slash command (e.g. /compact) the user ran — a slim marker, not a chat bubble -->
      <div v-else-if="entry.kind === 'command'" class="my-1.5 flex flex-col items-center gap-1">
        <div
          v-if="entry.name"
          class="inline-flex items-center gap-1.5 rounded-full bg-muted/60 border border-border px-2.5 py-1 text-[11px]"
        >
          <Terminal class="h-3 w-3 text-foreground/50" :stroke-width="1.75" />
          <span class="font-mono font-medium text-foreground/75">{{ entry.name }}</span>
          <span v-if="entry.args" class="font-mono text-foreground/55">{{ entry.args }}</span>
        </div>
        <div v-if="entry.stdout" class="text-[11px] font-mono text-foreground/45">{{ entry.stdout }}</div>
      </div>

      <!-- Assistant text -->
      <CollapsibleMessage
        v-else-if="entry.kind === 'text'"
        :max-height="384"
        fade="hsl(var(--background))"
        :disabled="expandedMessageIndices.has(i)"
      >
        <div
          v-file-links
          v-mermaid
          v-copy-code
          v-html="renderText(entry.text)"
          class="markdown-output text-sm leading-relaxed text-foreground"
          @click="onProseClick"
        />
      </CollapsibleMessage>

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

      <!-- Bash-family tool call — terminal-style card: purpose headline, the real
           command in a copyable terminal block, then its output. -->
      <div
        v-else-if="entry.kind === 'tool' && isBash(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error
          ? 'border-border border-l-red-500/70'
          : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button
          class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer"
          @click="toggle(i)"
        >
          <ChevronRight
            class="h-3 w-3 text-foreground/40 transition-transform shrink-0"
            :stroke-width="1.75"
            :class="{ 'rotate-90': expanded[i] }"
          />
          <Terminal class="h-3.5 w-3.5 shrink-0 text-emerald-400" :stroke-width="1.75" />
          <!-- Primary line: the command's purpose (description), or the command
               itself when no description was provided. -->
          <span class="text-xs text-foreground/80 truncate">{{ bashHeadline(entry.name, entry.input) }}</span>
          <span
            v-if="bashInfo(entry.input).background"
            class="text-[9px] uppercase tracking-wider text-foreground/40 border border-border rounded px-1 shrink-0"
          >bg</span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5">
            <XCircle class="h-3 w-3" :stroke-width="1.75" />error
          </span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5">
            <CheckCircle2 class="h-3 w-3" :stroke-width="1.75" />
          </span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">running…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border">
          <!-- Real command executed on the machine — terminal look + copy.
               Shell-management tools (BashOutput/KillShell) carry no command,
               so skip the block entirely rather than render an empty `$`. -->
          <div v-if="bashInfo(entry.input).command" class="px-3 py-2 bg-zinc-950/90 dark:bg-black/40 relative group/cmd">
            <div class="text-[10px] uppercase tracking-wider text-white/40 mb-1 flex items-center justify-between">
              <span>command</span>
              <button
                class="inline-flex items-center gap-1 text-white/60 hover:text-white cursor-pointer"
                :title="copiedCmd[i] ? 'Copied' : 'Copy command'"
                @click.stop="copyCommand(i, entry.input)"
              >
                <component :is="copiedCmd[i] ? Check : Copy" class="h-3 w-3" :stroke-width="1.75" />
                <span class="text-[9px]">{{ copiedCmd[i] ? 'copied' : 'copy' }}</span>
              </button>
            </div>
            <pre class="text-[11px] font-mono text-emerald-300/90 whitespace-pre-wrap break-words leading-relaxed"><span class="select-none text-emerald-500/60">$ </span>{{ bashInfo(entry.input).command }}</pre>
          </div>
          <!-- Command output (stdout/stderr). -->
          <div v-if="entry.result" class="px-3 py-2 border-t border-border">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">
              {{ entry.result.is_error ? 'error' : 'output' }}
            </div>
            <pre
              v-if="entry.result.content"
              class="text-[11px] font-mono whitespace-pre-wrap break-words max-h-96 overflow-auto"
              :class="entry.result.is_error ? 'text-red-600 dark:text-red-300/90' : 'text-foreground/70'"
            >{{ entry.result.content }}</pre>
            <div v-else class="text-[11px] font-mono text-foreground/30 italic">(no output)</div>
          </div>
        </div>
      </div>

      <!-- File-editing tool call (Edit / MultiEdit / Write) — unified diff view. -->
      <div
        v-else-if="entry.kind === 'tool' && isEditTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error
          ? 'border-border border-l-red-500/70'
          : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button
          class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer"
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
          <span
            v-if="fileTarget(entry.input)"
            class="text-[11px] font-mono text-sky-600 dark:text-sky-400/90 truncate underline decoration-dotted underline-offset-2 hover:text-sky-500 cursor-pointer"
            :title="'Open ' + fileTarget(entry.input)"
            role="button"
            @click.stop="emit('open-file', fileTarget(entry.input)!)"
          >{{ fileTarget(entry.input) }}</span>
          <!-- +added / −removed stat badge -->
          <span
            v-if="editStats(entry.name, entry.input).added || editStats(entry.name, entry.input).removed"
            class="text-[10px] font-mono shrink-0 flex items-center gap-1"
          >
            <span v-if="editStats(entry.name, entry.input).added" class="text-emerald-600 dark:text-emerald-400/80">+{{ editStats(entry.name, entry.input).added }}</span>
            <span v-if="editStats(entry.name, entry.input).removed" class="text-red-600 dark:text-red-400/80">−{{ editStats(entry.name, entry.input).removed }}</span>
          </span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5">
            <XCircle class="h-3 w-3" :stroke-width="1.75" />error
          </span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5">
            <CheckCircle2 class="h-3 w-3" :stroke-width="1.75" />
          </span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">running…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border p-2 space-y-2">
          <DiffView
            v-for="(part, pi) in editParts(entry.name, entry.input)"
            :key="pi"
            :before="part.before"
            :after="part.after"
          />
          <div v-if="entry.result?.is_error" class="px-1">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">error</div>
            <pre class="text-[11px] font-mono text-red-600 dark:text-red-300/90 whitespace-pre-wrap break-words max-h-60 overflow-auto">{{ entry.result.content }}</pre>
          </div>
        </div>
      </div>

      <!-- Sub-agent (Task): subagent + goal in header, its report rendered as markdown. -->
      <div
        v-else-if="entry.kind === 'tool' && isAgentTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error ? 'border-border border-l-red-500/70' : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <Bot class="h-3.5 w-3.5 shrink-0 text-fuchsia-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-fuchsia-600 dark:text-fuchsia-300">{{ agentInfo(entry.input).subagent || 'agent' }}</span>
          <span class="text-[11px] text-foreground/60 truncate">{{ agentInfo(entry.input).description }}</span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5"><XCircle class="h-3 w-3" :stroke-width="1.75" />error</span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5"><CheckCircle2 class="h-3 w-3" :stroke-width="1.75" /></span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">running…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border">
          <div v-if="agentInfo(entry.input).prompt" class="px-3 py-2 bg-black/20 dark:bg-black/25">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">task</div>
            <pre class="text-[11px] font-mono text-foreground/70 whitespace-pre-wrap break-words max-h-60 overflow-auto">{{ agentInfo(entry.input).prompt }}</pre>
          </div>
          <div v-if="entry.result" class="px-3 py-2 border-t border-border">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">{{ entry.result.is_error ? 'error' : 'result' }}</div>
            <div
              v-if="!entry.result.is_error"
              v-file-links v-mermaid v-copy-code
              v-html="renderText(entry.result.content)"
              class="markdown-output text-sm leading-relaxed text-foreground"
              @click="onProseClick"
            />
            <pre v-else class="text-[11px] font-mono text-red-600 dark:text-red-300/90 whitespace-pre-wrap break-words max-h-96 overflow-auto">{{ entry.result.content }}</pre>
          </div>
        </div>
      </div>

      <!-- AskUserQuestion: questions + options, and the user's answer. -->
      <div
        v-else-if="entry.kind === 'tool' && isAskUserTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error ? 'border-border border-l-red-500/70' : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <MessageCircleQuestion class="h-3.5 w-3.5 shrink-0 text-blue-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-blue-600 dark:text-blue-300">question</span>
          <span class="text-[11px] text-foreground/60 truncate">{{ askQuestions(entry.input)[0]?.question }}</span>
          <span v-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5"><CheckCircle2 class="h-3 w-3" :stroke-width="1.75" /></span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">waiting…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border px-3 py-2 space-y-3">
          <div v-for="(q, qi) in askQuestions(entry.input)" :key="qi" class="space-y-1.5">
            <div class="text-xs font-medium text-foreground/85">{{ q.question }}</div>
            <div v-for="(opt, oi) in q.options" :key="oi" class="flex gap-2 text-[11px] pl-2">
              <Circle class="h-3 w-3 mt-0.5 shrink-0 text-foreground/30" :stroke-width="1.75" />
              <div><span class="font-medium text-foreground/80">{{ opt.label }}</span><span v-if="opt.description" class="text-foreground/45"> — {{ opt.description }}</span></div>
            </div>
          </div>
          <div v-if="entry.result" class="pt-1 border-t border-border">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1 mt-2">answer</div>
            <pre class="text-[11px] font-mono text-emerald-700 dark:text-emerald-300/90 whitespace-pre-wrap break-words">{{ entry.result.content }}</pre>
          </div>
        </div>
      </div>

      <!-- TodoWrite: render the plan as a status checklist. -->
      <div
        v-else-if="entry.kind === 'tool' && isTodoTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <ListTodo class="h-3.5 w-3.5 shrink-0 text-blue-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-blue-600 dark:text-blue-300">todos</span>
          <span class="text-[11px] text-foreground/50 tabular-nums shrink-0">{{ todoStats(entry.input).done }}/{{ todoStats(entry.input).total }}</span>
          <span class="text-[11px] text-foreground/45 truncate">{{ todoItems(entry.input).find((t) => t.status === 'in_progress')?.content }}</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border px-3 py-2 space-y-1">
          <div v-for="(t, ti) in todoItems(entry.input)" :key="ti" class="flex items-start gap-2 text-[12px]">
            <CheckCircle2 v-if="t.status === 'completed'" class="h-3.5 w-3.5 mt-0.5 shrink-0 text-emerald-500" :stroke-width="1.75" />
            <CircleDot v-else-if="t.status === 'in_progress'" class="h-3.5 w-3.5 mt-0.5 shrink-0 text-amber-500 animate-pulse" :stroke-width="1.75" />
            <Circle v-else class="h-3.5 w-3.5 mt-0.5 shrink-0 text-foreground/30" :stroke-width="1.75" />
            <span :class="{
              'line-through text-foreground/40': t.status === 'completed',
              'text-foreground/85 font-medium': t.status === 'in_progress',
              'text-foreground/70': t.status === 'pending',
            }">{{ t.content }}</span>
          </div>
        </div>
      </div>

      <!-- ToolSearch: the search query + matched tools. -->
      <div
        v-else-if="entry.kind === 'tool' && isToolSearchTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error ? 'border-border border-l-red-500/70' : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <Search class="h-3.5 w-3.5 shrink-0 text-teal-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-teal-600 dark:text-teal-300">tool-search</span>
          <span class="text-[11px] font-mono text-foreground/50 truncate">{{ toolSearchQuery(entry.input) }}</span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5"><XCircle class="h-3 w-3" :stroke-width="1.75" />error</span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5"><CheckCircle2 class="h-3 w-3" :stroke-width="1.75" /></span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">searching…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border">
          <div v-if="entry.result" class="px-3 py-2">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">{{ entry.result.is_error ? 'error' : 'matches' }}</div>
            <pre class="text-[11px] font-mono whitespace-pre-wrap break-words max-h-96 overflow-auto" :class="entry.result.is_error ? 'text-red-600 dark:text-red-300/90' : 'text-foreground/70'">{{ entry.result.content }}</pre>
          </div>
        </div>
      </div>

      <!-- WebFetch / WebSearch: fetched content rendered as markdown. -->
      <div
        v-else-if="entry.kind === 'tool' && isWebTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error ? 'border-border border-l-red-500/70' : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <Globe class="h-3.5 w-3.5 shrink-0 text-cyan-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-cyan-600 dark:text-cyan-300">{{ entry.name.toLowerCase() === 'websearch' ? 'web-search' : 'web-fetch' }}</span>
          <span class="text-[11px] font-mono text-foreground/50 truncate">{{ previewInput(entry.input) }}</span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5"><XCircle class="h-3 w-3" :stroke-width="1.75" />error</span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5"><CheckCircle2 class="h-3 w-3" :stroke-width="1.75" /></span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">loading…</span>
        </button>
        <div v-if="expanded[i] && entry.result" class="border-t border-border px-3 py-2">
          <div
            v-if="!entry.result.is_error"
            v-file-links v-mermaid v-copy-code
            v-html="renderText(entry.result.content)"
            class="markdown-output text-sm leading-relaxed text-foreground max-h-[32rem] overflow-auto"
            @click="onProseClick"
          />
          <pre v-else class="text-[11px] font-mono text-red-600 dark:text-red-300/90 whitespace-pre-wrap break-words max-h-96 overflow-auto">{{ entry.result.content }}</pre>
        </div>
      </div>

      <!-- ExitPlanMode: Claude's proposed plan, rendered as markdown. -->
      <div
        v-else-if="entry.kind === 'tool' && isPlanTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <ClipboardList class="h-3.5 w-3.5 shrink-0 text-indigo-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-indigo-600 dark:text-indigo-300">plan</span>
          <span class="text-[11px] text-foreground/45 truncate">Proposed plan</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border px-3 py-2">
          <div
            v-file-links v-mermaid v-copy-code
            v-html="renderText(planContent(entry.input))"
            class="markdown-output text-sm leading-relaxed text-foreground"
            @click="onProseClick"
          />
        </div>
      </div>

      <!-- MCP tool / elicitation: server + action are surfaced up front. -->
      <div
        v-else-if="entry.kind === 'tool' && isMcpTool(entry.name)"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error ? 'border-border border-l-red-500/70' : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer" @click="toggle(i)">
          <ChevronRight class="h-3 w-3 text-foreground/40 transition-transform shrink-0" :stroke-width="1.75" :class="{ 'rotate-90': expanded[i] }" />
          <Server class="h-3.5 w-3.5 shrink-0 text-teal-400" :stroke-width="1.75" />
          <span class="text-xs font-mono font-semibold shrink-0 text-teal-600 dark:text-teal-300">mcp</span>
          <span class="text-[10px] font-mono shrink-0 rounded border border-border bg-foreground/4 px-1.5 py-0.5 text-foreground/55 truncate max-w-[10rem]" :title="mcpServerLabel(entry)">
            {{ mcpServerLabel(entry) }}
          </span>
          <span class="text-xs text-foreground/80 truncate">{{ mcpActionLabel(entry) }}</span>
          <span v-if="mcpSummary(entry)" class="text-[11px] text-foreground/45 truncate">{{ mcpSummary(entry) }}</span>
          <span v-if="entry.result?.is_error" class="ml-auto text-[10px] text-red-500 dark:text-red-400 shrink-0 flex items-center gap-0.5"><XCircle class="h-3 w-3" :stroke-width="1.75" />denied</span>
          <span v-else-if="entry.result" class="ml-auto text-[10px] text-emerald-600/80 dark:text-emerald-400/70 shrink-0 flex items-center gap-0.5"><CheckCircle2 class="h-3 w-3" :stroke-width="1.75" /></span>
          <span v-else class="ml-auto text-[10px] text-amber-500/80 dark:text-amber-400/70 animate-pulse shrink-0">waiting…</span>
        </button>
        <div v-if="expanded[i]" class="border-t border-border">
          <div class="grid grid-cols-[6rem_minmax(0,1fr)] gap-x-3 gap-y-1 px-3 py-2 bg-black/20 dark:bg-black/25 text-[11px]">
            <div class="uppercase tracking-wider text-foreground/35">server</div>
            <div class="font-mono text-foreground/75 truncate">{{ mcpServerLabel(entry) }}</div>
            <div class="uppercase tracking-wider text-foreground/35">action</div>
            <div class="font-mono text-foreground/75 truncate">{{ mcpActionLabel(entry) }}</div>
            <template v-if="mcpSummary(entry)">
              <div class="uppercase tracking-wider text-foreground/35">message</div>
              <div class="text-foreground/70 whitespace-pre-wrap break-words">{{ mcpSummary(entry) }}</div>
            </template>
          </div>
          <div class="px-3 py-2 border-t border-border">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">input</div>
            <pre class="text-[11px] font-mono text-foreground/70 whitespace-pre-wrap break-words max-h-60 overflow-auto">{{ prettyInput(entry.input) }}</pre>
          </div>
          <div v-if="entry.result" class="px-3 py-2 border-t border-border">
            <div class="text-[10px] uppercase tracking-wider text-foreground/40 mb-1">{{ entry.result.is_error ? 'status' : 'output' }}</div>
            <pre
              class="text-[11px] font-mono whitespace-pre-wrap break-words max-h-96 overflow-auto"
              :class="entry.result.is_error ? 'text-red-600 dark:text-red-300/90' : 'text-foreground/70'"
            >{{ entry.result.content }}</pre>
          </div>
        </div>
      </div>

      <!-- Tool call -->
      <div
        v-else-if="entry.kind === 'tool'"
        class="rounded-lg border border-l-[3px] bg-card/80 backdrop-blur-sm shadow-sm shadow-black/20 overflow-hidden"
        :class="entry.result?.is_error
          ? 'border-border border-l-red-500/70'
          : ['border-border', toolStyle(entry.name).barClass]"
      >
        <button
          class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-foreground/[0.07] transition-colors cursor-pointer"
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
          <div class="px-3 py-2 bg-black/20 dark:bg-black/25">
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

      <!-- Result summary — only shown on failure (or when it carries its own
           text). A successful turn's final message is already rendered as its
           own entry, so the "Completed" box would just be empty noise. -->
      <div
        v-else-if="entry.kind === 'result' && (entry.is_error || entry.text)"
        class="mt-2 rounded-md border p-3"
        :class="entry.is_error
          ? 'border-red-500/30 bg-red-500/5'
          : 'border-emerald-500/20 bg-emerald-500/5'"
      >
        <div v-if="entry.is_error" class="flex items-center gap-2 mb-2">
          <component :is="XCircle" class="h-4 w-4 text-red-400" :stroke-width="2" />
          <span class="text-xs font-medium text-red-300">Failed</span>
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
          v-mermaid
          v-copy-code
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
      class="thinking-indicator flex items-center gap-2 text-xs font-mono"
    >
      <span class="thinking-icon inline-flex">
        <Sparkles class="h-3 w-3 text-emerald-400" :stroke-width="1.75" />
      </span>
      <span class="thinking-text">thinking</span>
      <span class="thinking-dots" aria-hidden="true">
        <span class="thinking-dot"></span>
        <span class="thinking-dot"></span>
        <span class="thinking-dot"></span>
      </span>
    </div>
  </div>
</template>

<style scoped>
/* Icon: gentle breathing glow + subtle rotation to feel "alive". */
.thinking-icon {
  animation: thinking-icon-pulse 1.8s ease-in-out infinite;
  transform-origin: center;
}

@keyframes thinking-icon-pulse {
  0%, 100% {
    opacity: 0.55;
    transform: scale(0.92) rotate(-6deg);
    filter: drop-shadow(0 0 0 rgba(52, 211, 153, 0));
  }
  50% {
    opacity: 1;
    transform: scale(1.08) rotate(6deg);
    filter: drop-shadow(0 0 4px rgba(52, 211, 153, 0.5));
  }
}

/* Text: animated shimmer sweeping across the word. */
.thinking-text {
  background: linear-gradient(
    100deg,
    rgba(148, 163, 184, 0.45) 30%,
    rgba(52, 211, 153, 0.95) 50%,
    rgba(148, 163, 184, 0.45) 70%
  );
  background-size: 220% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  animation: thinking-shimmer 2s linear infinite;
}

@keyframes thinking-shimmer {
  0% { background-position: 180% 0; }
  100% { background-position: -80% 0; }
}

/* Trailing dots that bounce in sequence. */
.thinking-dots {
  display: inline-flex;
  align-items: flex-end;
  gap: 3px;
  margin-left: -2px;
  padding-bottom: 1px;
}

.thinking-dot {
  width: 3px;
  height: 3px;
  border-radius: 9999px;
  background-color: rgb(52, 211, 153);
  animation: thinking-bounce 1.4s ease-in-out infinite;
}

.thinking-dot:nth-child(2) {
  animation-delay: 0.2s;
}

.thinking-dot:nth-child(3) {
  animation-delay: 0.4s;
}

@keyframes thinking-bounce {
  0%, 80%, 100% {
    transform: translateY(0);
    opacity: 0.4;
  }
  40% {
    transform: translateY(-4px);
    opacity: 1;
  }
}

/* Respect reduced-motion preferences: keep it readable, drop the motion. */
@media (prefers-reduced-motion: reduce) {
  .thinking-icon,
  .thinking-text,
  .thinking-dot {
    animation: none;
  }
  .thinking-text {
    color: rgba(148, 163, 184, 0.7);
  }
}
</style>
