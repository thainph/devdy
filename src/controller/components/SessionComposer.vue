<script setup lang="ts">
/**
 * Controller composer (Phase 4 parity, ported from RunView regions ~2603-2827 +
 * supporting logic). Backend-agnostic: props in, events out. Parity scope is
 * DELIBERATELY limited to four features — engine/model selectors, permission
 * mode, slash-command palette and `@`-mention/attachments; context-meter,
 * budget and engine handoff are OUT of scope.
 *
 * Tauri-free: attachments are read via the browser {@link FileReader} into
 * base64 (no Tauri fs); `@`-mention paths come from the `projectFiles` prop and
 * are inserted as backticked paths (mirroring the desktop's literal-path style).
 */
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  Cpu, FileText, ImagePlus, Maximize2, Minimize2, Paperclip, Play, Send, Sparkles, Square, X,
} from 'lucide-vue-next'
import { Button, AppSelect } from '@/components/ui'

const { t } = useI18n()
import { modelOptionsFor, PERMISSION_MODE_OPTIONS, type SelectOption } from '@/lib/engineOptions'
import type { CmdAttachment, SlashCommand } from '../protocol'

/** One outbound turn assembled by the composer. */
export interface ComposerTurn {
  text: string
  mentions: string[]
  attachments: CmdAttachment[]
  /** Called by the transport owner only after the command was eligible and sent. */
  accept: () => void
}

const props = withDefaults(
  defineProps<{
    /** Slash commands to offer in the palette. */
    slashCommands?: SlashCommand[]
    /** Project file paths for `@`-mention autocomplete. */
    projectFiles?: string[]
    /** Engine selectors (empty = default). */
    engine?: string
    model?: string
    permissionMode?: string
    /** True while the bound run is executing (drives Send vs Run + Cancel). */
    running?: boolean
    /** True while a turn is being sent (disables the primary button). */
    sending?: boolean
    /** True when the connection isn't ready (disables everything). */
    disabled?: boolean
  }>(),
  {
    slashCommands: () => [],
    projectFiles: () => [],
    engine: '',
    model: '',
    permissionMode: '',
    running: false,
    sending: false,
    disabled: false,
  },
)

const emit = defineEmits<{
  send: [turn: ComposerTurn]
  cancel: []
  'update:engine': [value: string]
  'update:model': [value: string]
  'update:permissionMode': [value: string]
  /** Ask the parent to (re)fetch the project file list (opened a mention). */
  requestFiles: []
}>()

const MENTION_LIMIT = 50
const MAX_IMAGE_BYTES = 10 * 1024 * 1024

const text = ref('')
const composerEl = ref<HTMLTextAreaElement | null>(null)
const imageInputEl = ref<HTMLInputElement | null>(null)
const fileInputEl = ref<HTMLInputElement | null>(null)

// ── engine/model options ──────────────────────────────────────────────────
const engineOptions = computed<SelectOption[]>(() => [
  { value: '', label: t('controller.composer.defaultEngine') },
  { value: 'claude', label: 'claude' },
  { value: 'codex', label: 'codex' },
])
const modelOptions = computed(() => modelOptionsFor(props.engine || 'claude'))

function onEngineChange(next: string): void {
  emit('update:engine', next)
  // Reset the model when it isn't valid for the new engine (desktop parity).
  if (!modelOptionsFor(next || 'claude').some((o) => o.value === props.model)) {
    emit('update:model', '')
  }
}

// ── attachments (base64 images) + file references ─────────────────────────
interface PendingImage {
  id: string
  mime: string
  data: string // base64 (no data-URL prefix)
  url: string // data URL for the thumbnail
}
interface PendingFile {
  id: string
  name: string
  path: string
}
const pendingImages = ref<PendingImage[]>([])
const pendingFiles = ref<PendingFile[]>([])
let imageSeq = 0
let fileSeq = 0

function pushPendingImage(mime: string, data: string): void {
  pendingImages.value.push({ id: `img-${imageSeq++}`, mime, data, url: `data:${mime};base64,${data}` })
}

function addImageFile(file: File | null): void {
  if (!file || !file.type.startsWith('image/')) return
  if (file.size > MAX_IMAGE_BYTES) {
    // eslint-disable-next-line no-alert
    alert(t('controller.composer.imageTooLarge', { size: MAX_IMAGE_BYTES / 1024 / 1024 }))
    return
  }
  const reader = new FileReader()
  reader.onload = () => {
    const url = String(reader.result || '')
    const comma = url.indexOf(',')
    if (comma < 0) return
    pushPendingImage(file.type, url.slice(comma + 1))
  }
  reader.readAsDataURL(file)
}

function onPickImages(e: Event): void {
  const inp = e.target as HTMLInputElement
  for (const f of Array.from(inp.files ?? [])) addImageFile(f)
  inp.value = ''
}

function removePendingImage(id: string): void {
  pendingImages.value = pendingImages.value.filter((img) => img.id !== id)
}

// Non-image files are attached by name (embedded base64). The FileReader reads
// them the same way; the Host receives {name, mime, data_base64}.
const pendingFileData = new Map<string, CmdAttachment>()

function onPickFiles(e: Event): void {
  const inp = e.target as HTMLInputElement
  for (const f of Array.from(inp.files ?? [])) addAttachedFile(f)
  inp.value = ''
}

function addAttachedFile(file: File): void {
  if (file.type.startsWith('image/')) {
    addImageFile(file)
    return
  }
  if (file.size > MAX_IMAGE_BYTES) {
    // eslint-disable-next-line no-alert
    alert(t('controller.composer.fileTooLarge', { size: MAX_IMAGE_BYTES / 1024 / 1024 }))
    return
  }
  const id = `file-${fileSeq++}`
  const reader = new FileReader()
  reader.onload = () => {
    const url = String(reader.result || '')
    const comma = url.indexOf(',')
    if (comma < 0) return
    pendingFileData.set(id, {
      name: file.name,
      mime: file.type || 'application/octet-stream',
      data_base64: url.slice(comma + 1),
    })
    pendingFiles.value.push({ id, name: file.name, path: file.name })
  }
  reader.readAsDataURL(file)
}

function removePendingFile(id: string): void {
  pendingFiles.value = pendingFiles.value.filter((f) => f.id !== id)
  pendingFileData.delete(id)
}

function onComposerPaste(e: ClipboardEvent): void {
  const items = e.clipboardData?.items
  if (!items) return
  let handled = false
  for (const item of items) {
    if (item.kind === 'file' && item.type.startsWith('image/')) {
      const file = item.getAsFile()
      if (file) {
        addImageFile(file)
        handled = true
      }
    }
  }
  if (handled) e.preventDefault()
}

// ── auto-resize / expand ───────────────────────────────────────────────────
const composerExpanded = ref(false)

function autoResize(): void {
  const el = composerEl.value
  if (!el) return
  if (composerExpanded.value) {
    el.style.height = `${Math.round(window.innerHeight * 0.5)}px`
    el.style.overflowY = 'auto'
    return
  }
  const maxPx = 180
  el.style.height = 'auto'
  const next = Math.min(el.scrollHeight, maxPx)
  el.style.height = `${next}px`
  el.style.overflowY = el.scrollHeight > maxPx ? 'auto' : 'hidden'
}

function toggleExpand(): void {
  composerExpanded.value = !composerExpanded.value
  nextTick(() => {
    autoResize()
    composerEl.value?.focus()
  })
}

watch(text, () => autoResize(), { flush: 'post' })

// ── slash-command palette ──────────────────────────────────────────────────
const slashOpen = ref(false)
const slashQuery = ref('')
const slashIndex = ref(0)
const slashItems = computed(() => {
  const q = slashQuery.value.toLowerCase()
  return props.slashCommands
    .map((c) => c.name)
    .filter((name) => name.toLowerCase().startsWith(q))
    .slice(0, MENTION_LIMIT)
})

function detectSlash(): void {
  const t = composerEl.value?.value ?? text.value
  const m = /^\/([a-zA-Z0-9_-]*)$/.exec(t)
  if (!m || !props.slashCommands.length) {
    slashOpen.value = false
    return
  }
  slashQuery.value = m[1]
  slashIndex.value = 0
  slashOpen.value = true
}

function selectSlash(name: string): void {
  text.value = `/${name}`
  slashOpen.value = false
  nextTick(() => {
    const el = composerEl.value
    if (el) {
      el.focus()
      const pos = text.value.length
      el.setSelectionRange(pos, pos)
    }
  })
}

// ── @-mention autocomplete ─────────────────────────────────────────────────
const mentionOpen = ref(false)
const mentionQuery = ref('')
const mentionStart = ref(0)
const mentionIndex = ref(0)

function scoreMention(path: string, q: string): number {
  const lp = path.toLowerCase()
  const slash = lp.lastIndexOf('/')
  const base = slash >= 0 ? lp.slice(slash + 1) : lp
  if (base === q) return 1000
  if (base.startsWith(q)) return 900 - base.length
  const bi = base.indexOf(q)
  if (bi >= 0) return 700 - bi - base.length * 0.01
  const pi = lp.indexOf(q)
  if (pi >= 0) return 500 - pi - lp.length * 0.01
  let qi = 0
  for (let i = 0; i < lp.length && qi < q.length; i++) {
    if (lp[i] === q[qi]) qi++
  }
  if (qi === q.length) return 200 - lp.length * 0.01
  return -1
}

const mentionItems = computed<string[]>(() => {
  const q = mentionQuery.value.toLowerCase()
  const src = props.projectFiles
  if (!q) return src.slice(0, MENTION_LIMIT)
  const scored: Array<{ p: string; s: number }> = []
  for (const p of src) {
    const s = scoreMention(p, q)
    if (s >= 0) scored.push({ p, s })
  }
  scored.sort((a, b) => b.s - a.s)
  return scored.slice(0, MENTION_LIMIT).map((x) => x.p)
})

function detectMention(): void {
  const el = composerEl.value
  if (!el) {
    mentionOpen.value = false
    return
  }
  const caret = el.selectionStart ?? 0
  const t = el.value
  let i = caret - 1
  while (i >= 0) {
    const ch = t[i]
    if (ch === '@') break
    if (ch === ' ' || ch === '\n' || ch === '\t') {
      mentionOpen.value = false
      return
    }
    i--
  }
  if (i < 0 || t[i] !== '@') {
    mentionOpen.value = false
    return
  }
  const prev = i > 0 ? t[i - 1] : ''
  if (prev && !/\s/.test(prev)) {
    mentionOpen.value = false
    return
  }
  const wasOpen = mentionOpen.value
  mentionStart.value = i
  mentionQuery.value = t.slice(i + 1, caret)
  mentionIndex.value = 0
  mentionOpen.value = true
  // Ask the parent to (re)fetch files when a mention session first opens.
  if (!wasOpen) emit('requestFiles')
}

function selectMention(path: string): void {
  const el = composerEl.value
  const caret = el?.selectionStart ?? text.value.length
  const before = text.value.slice(0, mentionStart.value)
  const after = text.value.slice(caret)
  const insert = `\`${path}\` `
  text.value = before + insert + after
  mentionOpen.value = false
  nextTick(() => {
    const pos = before.length + insert.length
    if (el) {
      el.focus()
      el.setSelectionRange(pos, pos)
    }
  })
}

function onInput(): void {
  detectMention()
  detectSlash()
}

// ── primary action / labels ────────────────────────────────────────────────
const hasContent = computed(
  () => text.value.trim().length > 0 || pendingImages.value.length > 0 || pendingFiles.value.length > 0,
)

const sendKind = computed<'sending' | 'send' | 'resume' | 'run'>(() => {
  if (props.sending) return 'sending'
  if (props.running) return 'send'
  return hasContent.value ? 'resume' : 'run'
})

const sendLabel = computed(() => {
  switch (sendKind.value) {
    case 'sending':
      return t('controller.composer.sending')
    case 'send':
      return t('controller.composer.send')
    case 'resume':
      return t('controller.composer.resume')
    default:
      return t('controller.composer.run')
  }
})

const primaryDisabled = computed(() => {
  if (props.disabled || props.sending) return true
  // A "Run" with no content still triggers the default prompt on the host.
  if (!props.running) return false
  return !hasContent.value
})

const placeholder = computed(() =>
  props.running
    ? t('controller.composer.placeholderRunning')
    : t('controller.composer.placeholderIdle'),
)

function collectMentions(): string[] {
  // Extract backticked paths that match a known project file.
  const known = new Set(props.projectFiles)
  const out: string[] = []
  const re = /`([^`]+)`/g
  let m: RegExpExecArray | null
  while ((m = re.exec(text.value))) {
    const p = m[1].trim().replace(/\/$/, '')
    if (known.has(p) && !out.includes(p)) out.push(p)
  }
  return out
}

function submit(): void {
  if (primaryDisabled.value) return
  const attachments: CmdAttachment[] = [
    ...pendingImages.value.map((img) => ({ name: `${img.id}`, mime: img.mime, data_base64: img.data })),
    ...pendingFiles.value.map((f) => pendingFileData.get(f.id)).filter((a): a is CmdAttachment => !!a),
  ]
  let accepted = false
  const accept = (): void => {
    if (accepted) return
    accepted = true
    text.value = ''
    pendingImages.value = []
    pendingFiles.value = []
    pendingFileData.clear()
    mentionOpen.value = false
    slashOpen.value = false
    nextTick(autoResize)
  }
  // Keep the draft and attachments until the connection confirms this command
  // was eligible for the authenticated ACTIVE channel and WebSocket.send did
  // not fail synchronously.
  emit('send', { text: text.value.trim(), mentions: collectMentions(), attachments, accept })
}

function onKeydown(e: KeyboardEvent): void {
  if (slashOpen.value && slashItems.value.length) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      slashIndex.value = (slashIndex.value + 1) % slashItems.value.length
      return
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault()
      slashIndex.value = (slashIndex.value - 1 + slashItems.value.length) % slashItems.value.length
      return
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      e.preventDefault()
      selectSlash(slashItems.value[slashIndex.value])
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      slashOpen.value = false
      return
    }
  }
  if (mentionOpen.value && mentionItems.value.length) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      mentionIndex.value = (mentionIndex.value + 1) % mentionItems.value.length
      return
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault()
      mentionIndex.value = (mentionIndex.value - 1 + mentionItems.value.length) % mentionItems.value.length
      return
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      e.preventDefault()
      selectMention(mentionItems.value[mentionIndex.value])
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      mentionOpen.value = false
      return
    }
  }
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    submit()
  }
}
</script>

<template>
  <div class="border-t border-border bg-card/40 px-3 py-2 shrink-0">
    <div class="relative">
      <!-- Slash-command palette -->
      <div
        v-if="slashOpen && slashItems.length"
        class="absolute bottom-full left-0 mb-1 max-h-60 w-72 max-w-[80vw] overflow-y-auto rounded-md border border-border bg-popover shadow-lg z-20"
      >
        <ul class="py-1 text-xs">
          <li
            v-for="(cmd, i) in slashItems"
            :key="cmd"
            class="flex items-center gap-2 px-3 py-1.5 cursor-pointer font-mono"
            :class="i === slashIndex ? 'bg-primary/15 text-foreground' : 'text-foreground/80 hover:bg-muted'"
            @mousedown.prevent="selectSlash(cmd)"
            @mouseenter="slashIndex = i"
          >
            <span class="text-primary">/</span>
            <span class="truncate">{{ cmd }}</span>
          </li>
        </ul>
      </div>

      <!-- @-mention autocomplete -->
      <div
        v-if="mentionOpen && mentionItems.length"
        class="absolute bottom-full left-0 mb-1 max-h-60 w-[28rem] max-w-[80vw] overflow-y-auto rounded-md border border-border bg-popover shadow-lg z-20"
      >
        <ul class="py-1 text-xs">
          <li
            v-for="(item, i) in mentionItems"
            :key="item"
            class="flex items-center gap-2 px-3 py-1.5 cursor-pointer font-mono"
            :class="i === mentionIndex ? 'bg-primary/15 text-foreground' : 'text-foreground/80 hover:bg-muted'"
            @mousedown.prevent="selectMention(item)"
            @mouseenter="mentionIndex = i"
          >
            <FileText class="h-3.5 w-3.5 shrink-0 opacity-60" :stroke-width="2" />
            <span class="truncate">{{ item }}</span>
          </li>
        </ul>
      </div>

      <!-- Image thumbnails -->
      <div v-if="pendingImages.length" class="flex flex-wrap gap-2 mb-2">
        <div
          v-for="img in pendingImages"
          :key="img.id"
          class="relative h-16 w-16 rounded-md border border-border overflow-hidden group"
        >
          <img :src="img.url" class="h-full w-full object-cover" alt="attachment" />
          <button
            class="absolute top-0.5 right-0.5 h-4 w-4 rounded-full bg-background/80 border border-border flex items-center justify-center text-foreground/70 hover:text-foreground hover:bg-background cursor-pointer opacity-0 group-hover:opacity-100 transition-opacity"
            :title="t('controller.composer.removeImage')"
            @click="removePendingImage(img.id)"
          >
            <X class="h-2.5 w-2.5" :stroke-width="2.5" />
          </button>
        </div>
      </div>

      <!-- File chips -->
      <div v-if="pendingFiles.length" class="flex flex-wrap gap-1.5 mb-2">
        <div
          v-for="file in pendingFiles"
          :key="file.id"
          class="group flex items-center gap-1.5 h-7 pl-2 pr-1 rounded-md border border-border bg-muted/40 text-xs max-w-64"
          :title="file.name"
        >
          <FileText class="h-3.5 w-3.5 shrink-0 opacity-60" :stroke-width="2" />
          <span class="truncate font-mono">{{ file.name }}</span>
          <button
            class="h-4 w-4 rounded-full flex items-center justify-center text-foreground/50 hover:text-foreground hover:bg-background cursor-pointer shrink-0"
            :title="t('controller.composer.removeFile')"
            @click="removePendingFile(file.id)"
          >
            <X class="h-2.5 w-2.5" :stroke-width="2.5" />
          </button>
        </div>
      </div>

      <input ref="imageInputEl" type="file" accept="image/*" multiple class="hidden" @change="onPickImages" />
      <input ref="fileInputEl" type="file" multiple class="hidden" @change="onPickFiles" />

      <!-- Prompt card -->
      <div class="relative rounded-lg border border-border bg-background focus-within:ring-1 focus-within:ring-ring transition-colors">
        <textarea
          ref="composerEl"
          v-model="text"
          rows="2"
          :placeholder="placeholder"
          class="w-full resize-none px-3 pt-2.5 pb-1 bg-transparent text-xs focus:outline-none font-mono min-h-[2.75rem] leading-relaxed"
          :disabled="disabled || sending"
          @keydown="onKeydown"
          @input="onInput"
          @click="onInput"
          @compositionend="onInput"
          @paste="onComposerPaste"
          @blur="mentionOpen = false; slashOpen = false"
        />

        <!-- Footer toolbar — two rows so nothing overflows on a phone: the
             selectors share the width (flex-1 + min-w-0), actions sit below. -->
        <div class="px-1.5 pb-1.5 space-y-1.5">
          <!-- Selector row: each select shares the width and never overflows. -->
          <div class="flex items-center gap-1.5">
            <AppSelect
              :model-value="engine"
              size="sm"
              variant="ghost"
              :options="engineOptions"
              class="flex-1 min-w-0 h-8"
              @update:model-value="onEngineChange"
            >
              <template #leading>
                <Cpu class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
              </template>
            </AppSelect>
            <AppSelect
              :model-value="permissionMode"
              size="sm"
              variant="ghost"
              :options="PERMISSION_MODE_OPTIONS"
              :disabled="running"
              class="flex-1 min-w-0 h-8"
              :title="t('controller.composer.permTitle')"
              @update:model-value="emit('update:permissionMode', $event)"
            >
              <template #leading>
                <span class="text-[10px] font-mono text-muted-foreground shrink-0">perm</span>
              </template>
            </AppSelect>
            <AppSelect
              :model-value="model"
              size="sm"
              variant="ghost"
              :options="modelOptions"
              :disabled="running"
              class="flex-1 min-w-0 h-8"
              :title="t('controller.composer.modelTitle')"
              @update:model-value="emit('update:model', $event)"
            >
              <template #leading>
                <Sparkles class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
              </template>
            </AppSelect>
          </div>

          <!-- Action row: attachments/expand on the left, Send/Cancel on the right. -->
          <div class="flex items-center gap-1.5">
            <button
              class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
              :disabled="disabled || sending"
              :title="t('controller.composer.attachImage')"
              @click="imageInputEl?.click()"
            >
              <ImagePlus class="h-4 w-4" :stroke-width="2" />
            </button>
            <button
              class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
              :disabled="disabled || sending"
              :title="t('controller.composer.attachFile')"
              @click="fileInputEl?.click()"
            >
              <Paperclip class="h-4 w-4" :stroke-width="2" />
            </button>
            <button
              class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
              :title="composerExpanded ? t('controller.composer.collapseComposer') : t('controller.composer.expandComposer')"
              @click="toggleExpand"
            >
              <component :is="composerExpanded ? Minimize2 : Maximize2" class="h-4 w-4" :stroke-width="2" />
            </button>

            <div class="ml-auto flex items-center gap-1.5 shrink-0">
              <Button
                v-if="running"
                variant="destructive"
                class="h-8 shrink-0"
                :title="t('controller.composer.cancelTitle')"
                @click="emit('cancel')"
              >
                <Square class="h-3.5 w-3.5" :stroke-width="2" fill="currentColor" />
                {{ t('controller.composer.cancel') }}
              </Button>
              <button
                class="inline-flex items-center justify-center gap-1.5 h-8 px-3.5 text-xs bg-primary text-primary-foreground rounded-md hover:opacity-90 transition-opacity cursor-pointer disabled:opacity-50 shrink-0"
                :disabled="primaryDisabled"
                @click="submit"
              >
                <component :is="sendKind === 'run' ? Play : Send" class="h-3.5 w-3.5" :stroke-width="2" />
                {{ sendLabel }}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
