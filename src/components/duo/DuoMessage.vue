<script setup lang="ts">
/**
 * One block in the merged Duo timeline — either a completed turn or the turn
 * currently in flight.
 *
 * A completed turn renders just its final markdown answer (cheap, readable);
 * the full stream (tool calls, thinking, results) is mounted lazily behind the
 * "details" toggle, so a long duo doesn't pay for dozens of StreamLogs. The
 * in-flight turn always shows the full stream — that's the interesting part
 * while you're watching it work.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  ChevronDown, ClipboardCopy, Check, ExternalLink, Loader2,
  ShieldQuestion, MessageCircleQuestion,
} from 'lucide-vue-next'
import StreamLog from '@/components/StreamLog.vue'
import { CollapsibleMessage } from '@/components/ui'
import { vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import type { DuoMessage } from '@/lib/duoConversation'

const props = defineProps<{
  message: Extract<DuoMessage, { kind: 'turn' | 'live' }>
  renderText: (md: string) => string
  /** Tool name of this side's pending permission request, if any — drives the
   * same "waiting for you" marker the session History list uses. */
  pendingTool?: string | null
}>()

const emit = defineEmits<{ openSession: [runId: string] }>()

const { t } = useI18n()

const isLive = computed(() => props.message.kind === 'live')
const isA = computed(() => props.message.role === 'designer')
const initial = computed(() => (props.message.label.trim()[0] || '?').toUpperCase())
const detailAvailable = computed(
  () => props.message.kind === 'turn' && !!props.message.entries?.length,
)
const showDetail = ref(false)

const time = computed(() => {
  const at = props.message.kind === 'turn' ? props.message.at : undefined
  if (!at) return ''
  const d = new Date(at)
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
})

const copied = ref(false)
async function copy() {
  if (props.message.kind !== 'turn') return
  try {
    await navigator.clipboard.writeText(props.message.text)
    copied.value = true
    setTimeout(() => { copied.value = false }, 1500)
  } catch { /* clipboard unavailable — nothing useful to say */ }
}
</script>

<template>
  <article
    class="rounded-lg border-l-2 bg-card/40 px-4 py-3"
    :class="isA ? 'border-primary/60' : 'border-violet-500/60'"
  >
    <header class="flex items-center gap-2 mb-2">
      <span
        class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold"
        :class="isA ? 'bg-primary/10 text-primary' : 'bg-violet-500/10 text-violet-600 dark:text-violet-400'"
      >{{ initial }}</span>
      <span class="text-xs font-semibold truncate">{{ message.label }}</span>
      <span class="text-[11px] font-mono text-muted-foreground shrink-0">{{ message.engine }}</span>
      <span class="text-[10px] text-muted-foreground/70 shrink-0">{{ t('duo.conversation.roundShort', { n: message.round }) }}</span>
      <span v-if="time" class="text-[10px] text-muted-foreground/70 shrink-0">{{ time }}</span>

      <Loader2 v-if="isLive && !pendingTool" class="h-3.5 w-3.5 animate-spin text-primary shrink-0" />
      <!-- Same attention marker the session History list uses for a run that
           needs an answer, so the two screens read alike. -->
      <span
        v-if="pendingTool"
        class="relative flex h-4 w-4 shrink-0 items-center justify-center text-primary"
        :title="pendingTool === 'AskUserQuestion' ? t('run.waitingForAnswer') : t('run.waitingForPermission')"
      >
        <span class="absolute inset-0 animate-ping rounded-full bg-primary/30" />
        <component
          :is="pendingTool === 'AskUserQuestion' ? MessageCircleQuestion : ShieldQuestion"
          class="relative h-4 w-4"
          :stroke-width="2"
        />
      </span>

      <div class="ml-auto flex items-center gap-0.5 shrink-0">
        <button
          v-if="message.kind === 'turn'"
          class="inline-flex h-6 w-6 items-center justify-center rounded text-foreground/40 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
          :title="t('duo.conversation.copyTurn')"
          @click="copy"
        >
          <Check v-if="copied" class="h-3.5 w-3.5 text-emerald-500" :stroke-width="2" />
          <ClipboardCopy v-else class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
        <button
          v-if="detailAvailable"
          class="inline-flex items-center gap-1 h-6 px-1.5 rounded text-[11px] text-foreground/50 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
          @click="showDetail = !showDetail"
        >
          <ChevronDown class="h-3.5 w-3.5 transition-transform" :class="showDetail ? 'rotate-180' : ''" :stroke-width="2" />
          {{ showDetail ? t('duo.conversation.hideDetail') : t('duo.conversation.detail') }}
        </button>
        <button
          v-if="message.runId"
          class="inline-flex h-6 w-6 items-center justify-center rounded text-foreground/40 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
          :title="t('duo.openSession')"
          @click="emit('openSession', message.runId!)"
        >
          <ExternalLink class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </div>
    </header>

    <!-- In-flight turn: the live stream IS the content. -->
    <StreamLog
      v-if="message.kind === 'live'"
      :entries="message.entries"
      :running="true"
      :render-text="renderText"
    />

    <template v-else>
      <CollapsibleMessage :max-height="480" fade="hsl(var(--card))">
        <div
          v-mermaid
          v-copy-code
          v-html="renderText(message.text)"
          class="markdown-output text-sm leading-relaxed text-foreground"
        />
      </CollapsibleMessage>
      <div v-if="showDetail && message.entries" class="mt-3 border-t border-border/60 pt-3">
        <StreamLog :entries="message.entries" :render-text="renderText" />
      </div>
    </template>
  </article>
</template>
