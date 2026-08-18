<script setup lang="ts">
// Floating result card for the "translate selection" feature. Given the selected
// text and a viewport anchor, it calls the `translate_text` backend command
// (one-shot LLM through the run sidecar) and renders the translation as markdown.
// The trigger button that opens this popover lives in RunView; this component
// owns the request lifecycle, the language quick-toggle, copy, and dismissal.
import { ref, onMounted, onUnmounted, computed, nextTick, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Languages, Copy, Check, X, Loader2, RefreshCw } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { useMarkdown } from '@/lib/markdown'

const { t } = useI18n()

const props = defineProps<{
  text: string
  x: number
  y: number
  initialLang: string
}>()

const emit = defineEmits<{ (e: 'close'): void }>()

const LANG_OPTIONS = [
  { value: 'vi', label: 'Tiếng Việt' },
  { value: 'en', label: 'English' },
  { value: 'ja', label: '日本語' },
  { value: 'zh', label: '中文' },
  { value: 'ko', label: '한국어' },
]

const { renderText, loadMarkdown } = useMarkdown()

const rootEl = ref<HTMLElement | null>(null)
const lang = ref(props.initialLang || 'vi')
const phase = ref<'loading' | 'done' | 'error'>('loading')
const result = ref('')
const errorMsg = ref('')
const copied = ref(false)

// Keep the card inside the viewport: clamp to the right/bottom edges.
const POPOVER_WIDTH = 380
const style = computed(() => {
  const left = Math.max(8, Math.min(props.x, window.innerWidth - POPOVER_WIDTH - 8))
  const top = Math.max(8, Math.min(props.y, window.innerHeight - 80))
  return { left: `${left}px`, top: `${top}px`, width: `${POPOVER_WIDTH}px` }
})

const renderedHtml = computed(() => (result.value ? renderText(result.value) : ''))

async function runTranslate() {
  phase.value = 'loading'
  errorMsg.value = ''
  result.value = ''
  try {
    const out = await invoke<string>('translate_text', {
      text: props.text,
      targetLang: lang.value,
    })
    result.value = out
    phase.value = 'done'
  } catch (e) {
    errorMsg.value = String(e)
    phase.value = 'error'
  }
}

function setLang(next: string) {
  if (next === lang.value) return
  lang.value = next
  runTranslate()
}

async function copyResult() {
  if (!result.value) return
  try {
    await navigator.clipboard.writeText(result.value)
    copied.value = true
    setTimeout(() => { copied.value = false }, 1500)
  } catch {
    // clipboard denied — ignore
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') { e.stopPropagation(); emit('close') }
}

function onDocMouseDown(e: MouseEvent) {
  if (rootEl.value && !rootEl.value.contains(e.target as Node)) emit('close')
}

onMounted(() => {
  loadMarkdown()
  runTranslate()
  nextTick(() => {
    document.addEventListener('keydown', onKeydown, true)
    // Defer the outside-click listener so the click that opened us doesn't close it.
    setTimeout(() => document.addEventListener('mousedown', onDocMouseDown, true), 0)
  })
})

onUnmounted(() => {
  document.removeEventListener('keydown', onKeydown, true)
  document.removeEventListener('mousedown', onDocMouseDown, true)
  // Best-effort: kill any still-running sidecar for this popover.
  void invoke('cancel_translate').catch(() => {})
})

// If the parent swaps the selected text while open, re-translate.
watch(() => props.text, () => runTranslate())
</script>

<template>
  <Teleport to="body">
    <div
      ref="rootEl"
      class="fixed z-[70] rounded-xl border border-border bg-card shadow-xl shadow-black/30 overflow-hidden flex flex-col"
      :style="style"
    >
      <!-- Header -->
      <div class="flex items-center gap-2 px-3 py-2 border-b border-border bg-muted/40">
        <Languages class="h-3.5 w-3.5 text-primary/80 shrink-0" :stroke-width="1.75" />
        <span class="text-[11px] font-semibold text-foreground/80">{{ t('misc.translate.heading') }}</span>
        <div class="ml-auto flex items-center gap-0.5">
          <button
            v-for="opt in LANG_OPTIONS"
            :key="opt.value"
            class="text-[10px] px-1.5 py-0.5 rounded transition-colors cursor-pointer"
            :class="opt.value === lang
              ? 'bg-primary/15 text-primary font-medium'
              : 'text-foreground/50 hover:text-foreground/80 hover:bg-foreground/5'"
            @click="setLang(opt.value)"
          >{{ opt.label }}</button>
        </div>
        <button
          class="ml-1 shrink-0 text-foreground/40 hover:text-foreground/80 transition-colors cursor-pointer"
          :title="t('misc.translate.close')"
          @click="emit('close')"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </div>

      <!-- Body -->
      <div class="px-3 py-2.5 max-h-[50vh] overflow-auto">
        <div v-if="phase === 'loading'" class="flex items-center gap-2 text-xs text-foreground/50 py-2">
          <Loader2 class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          {{ t('misc.translate.translating') }}
        </div>
        <div v-else-if="phase === 'error'" class="text-xs text-red-500 dark:text-red-400 space-y-2">
          <p class="whitespace-pre-wrap break-words">{{ errorMsg }}</p>
          <button
            class="inline-flex items-center gap-1 text-[11px] text-primary hover:underline cursor-pointer"
            @click="runTranslate"
          >
            <RefreshCw class="h-3 w-3" :stroke-width="2" />{{ t('misc.translate.retry') }}
          </button>
        </div>
        <div
          v-else
          v-html="renderedHtml"
          class="markdown-output text-sm leading-relaxed text-foreground"
        />
      </div>

      <!-- Footer -->
      <div v-if="phase === 'done'" class="flex items-center justify-end px-3 py-1.5 border-t border-border bg-muted/20">
        <button
          class="inline-flex items-center gap-1 text-[11px] text-foreground/60 hover:text-foreground transition-colors cursor-pointer"
          @click="copyResult"
        >
          <component :is="copied ? Check : Copy" class="h-3 w-3" :stroke-width="1.75" />
          {{ copied ? t('misc.translate.copied') : t('misc.translate.copy') }}
        </button>
      </div>
    </div>
  </Teleport>
</template>
