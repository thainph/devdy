<script lang="ts">
interface Draft {
  source: string
  english: string
  /** `source` the current `english` was translated from (null = none yet). */
  translatedFrom: string | null
}

/** Per-thread drafts, module-wide so they survive the pane re-mounting. */
const drafts = new Map<string, Draft>()
</script>

<script setup lang="ts">
// Reply box under a Slack thread. The user writes in Vietnamese; once typing
// pauses the draft is translated into English, shown in an editable preview,
// and only that English text is posted when the user presses Send. A draft
// whose translation is stale (source edited since) cannot be sent.
//
// Auto-translate can be switched off (remembered in localStorage); the source
// text is then posted verbatim.
//
// Drafts are kept per thread for the lifetime of the window.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Languages, Loader2, SendHorizontal } from 'lucide-vue-next'
import { Button, Textarea } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { errorCode, useSlackThreadsStore } from '@/stores/slackThreads'
import { useSlackText } from './useSlackText'

const props = defineProps<{ threadId: string }>()

/** Typing pause before the draft is translated. */
const TRANSLATE_DELAY_MS = 900
const AUTO_TRANSLATE_KEY = 'devdy.slackReply.autoTranslate'

const { t } = useI18n()
const { toast } = useToast()
const { errorText } = useSlackText()
const store = useSlackThreadsStore()

const source = ref('')
const english = ref('')
const translatedFrom = ref<string | null>(null)
const translating = ref(false)
const translateError = ref<string | null>(null)
const sending = ref(false)
const autoTranslate = ref(localStorage.getItem(AUTO_TRANSLATE_KEY) !== '0')

let timer: ReturnType<typeof setTimeout> | null = null
/** Bumped per request so a late answer for an older draft is dropped. */
let requestSeq = 0

const trimmed = computed(() => source.value.trim())
const stale = computed(() => !!trimmed.value && translatedFrom.value !== trimmed.value)
const canSend = computed(() =>
  autoTranslate.value
    ? !!english.value.trim() && !stale.value && !translating.value && !sending.value
    : !!trimmed.value && !sending.value,
)

function toggleAutoTranslate() {
  autoTranslate.value = !autoTranslate.value
  localStorage.setItem(AUTO_TRANSLATE_KEY, autoTranslate.value ? '1' : '0')
  if (autoTranslate.value) {
    translate()
  } else {
    // Drop any pending/in-flight translation.
    if (timer) clearTimeout(timer)
    requestSeq++
    translating.value = false
    translateError.value = null
  }
}

function save(id: string) {
  if (source.value || english.value) {
    drafts.set(id, {
      source: source.value,
      english: english.value,
      translatedFrom: translatedFrom.value,
    })
  } else {
    drafts.delete(id)
  }
}

function load(id: string) {
  const d = drafts.get(id)
  source.value = d?.source ?? ''
  english.value = d?.english ?? ''
  translatedFrom.value = d?.translatedFrom ?? null
  translateError.value = null
  translating.value = false
  requestSeq++
}

watch(
  () => props.threadId,
  (id, prev) => {
    if (timer) clearTimeout(timer)
    if (prev) save(prev)
    load(id)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  if (timer) clearTimeout(timer)
  save(props.threadId)
})

async function translate() {
  if (timer) clearTimeout(timer)
  if (!autoTranslate.value) return
  const text = trimmed.value
  if (!text) {
    english.value = ''
    translatedFrom.value = null
    translateError.value = null
    return
  }
  if (text === translatedFrom.value) return
  const seq = ++requestSeq
  translating.value = true
  translateError.value = null
  try {
    const out = await store.translateReply(text)
    if (seq !== requestSeq) return
    english.value = out
    translatedFrom.value = text
  } catch (e) {
    if (seq !== requestSeq) return
    const { code, detail } = errorCode(e)
    translateError.value = errorText(code, detail)
  } finally {
    if (seq === requestSeq) translating.value = false
  }
}

function onSourceInput(value: string) {
  source.value = value
  if (timer) clearTimeout(timer)
  if (autoTranslate.value) timer = setTimeout(translate, TRANSLATE_DELAY_MS)
}

async function send() {
  if (!canSend.value) return
  const id = props.threadId
  sending.value = true
  try {
    await store.postReply(id, autoTranslate.value ? english.value.trim() : trimmed.value)
    toast.success(t('slackThread.reply.sent'))
    drafts.delete(id)
    if (id === props.threadId) load(id)
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(t('slackThread.reply.sendFailed', { error: errorText(code, detail) }))
  } finally {
    sending.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    if (autoTranslate.value && stale.value) translate()
    else send()
  }
}
</script>

<template>
  <div class="shrink-0 border-t border-border bg-background px-4 py-3" @keydown="onKeydown">
    <Textarea
      :model-value="source"
      rows="2"
      class="max-h-40 min-h-[3.25rem]"
      :placeholder="
        autoTranslate ? t('slackThread.reply.placeholder') : t('slackThread.reply.placeholderRaw')
      "
      :disabled="sending"
      @update:model-value="onSourceInput"
      @blur="translate"
    />

    <div v-if="autoTranslate && (trimmed || english)" class="mt-2 space-y-1">
      <div class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
        <Languages class="h-3.5 w-3.5" />
        <span class="font-medium">{{ t('slackThread.reply.englishLabel') }}</span>
        <span v-if="translating" class="inline-flex items-center gap-1">
          <Loader2 class="h-3 w-3 animate-spin" />
          {{ t('slackThread.reply.translating') }}
        </span>
        <span v-else-if="stale && !translateError">{{ t('slackThread.reply.stale') }}</span>
      </div>
      <p v-if="translateError" class="flex items-center gap-2 text-xs text-destructive">
        {{ translateError }}
        <Button variant="ghost" size="xs" @click="translate">{{ t('slackThread.reply.retry') }}</Button>
      </p>
      <Textarea
        v-model="english"
        rows="2"
        class="max-h-48 min-h-[3.25rem]"
        :placeholder="t('slackThread.reply.englishPlaceholder')"
        :disabled="sending || translating"
      />
    </div>

    <div class="mt-2 flex items-center justify-between gap-2">
      <div class="flex min-w-0 items-center gap-3">
        <button
          type="button"
          role="switch"
          :aria-checked="autoTranslate"
          class="flex shrink-0 items-center gap-1.5 rounded-md text-[11px] text-muted-foreground cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          :disabled="sending"
          @click="toggleAutoTranslate"
        >
          <span
            class="relative inline-flex h-4 w-7 shrink-0 items-center rounded-full transition-colors"
            :class="autoTranslate ? 'bg-primary' : 'bg-muted'"
          >
            <span
              class="inline-block h-3 w-3 rounded-full bg-white shadow transition-transform"
              :class="autoTranslate ? 'translate-x-3.5' : 'translate-x-0.5'"
            />
          </span>
          {{ t('slackThread.reply.autoTranslate') }}
        </button>
        <span class="truncate text-[11px] text-muted-foreground">
          {{ autoTranslate ? t('slackThread.reply.hint') : t('slackThread.reply.hintRaw') }}
        </span>
      </div>
      <Button size="sm" :disabled="!canSend" @click="send">
        <Loader2 v-if="sending" class="h-3.5 w-3.5 animate-spin" />
        <SendHorizontal v-else class="h-3.5 w-3.5" />
        {{ t('slackThread.reply.send') }}
      </Button>
    </div>
  </div>
</template>
