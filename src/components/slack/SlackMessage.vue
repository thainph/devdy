<script setup lang="ts">
// One Slack message, laid out like Slack: 36px avatar, bold name (+ APP badge for
// bots), HH:mm time (full date on hover), the mrkdwn body, "(edited)", files and
// reaction chips. A `continued` message (same sender ≤ 5 min after the previous)
// hides avatar + name and shows its time in the gutter on hover.
//
// The body is the Vietnamese text or the original, per the window's toggle or
// this message's own override; without a translation the original shows with a
// "Not translated / Translating… / Translation failed" tag. Hover actions:
// show original / translation (this message only) and copy (plain text, mentions
// as @Name).
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Check, Copy, Languages } from 'lucide-vue-next'
import { useToast } from '@/composables/useToast'
import { emojiFor } from '@/lib/slackEmoji'
import { mrkdwnToPlain, renderSlackMrkdwn, type MrkdwnContext } from '@/lib/slackMrkdwn'
import { formatClock, formatFullDate } from '@/lib/slackThreadUtils'
import type { SlackFile, SlackMessage, SlackUser } from '@/stores/slackThreads'
import SlackAvatar from './SlackAvatar.vue'
import SlackMessageFiles from './SlackMessageFiles.vue'

const props = defineProps<{
  message: SlackMessage
  continued: boolean
  ctx: MrkdwnContext
  /** Sender's user row, when known (bots usually have none). */
  user: SlackUser | null
  files: SlackFile[]
  /** Show the original (window toggle XOR this message's override). */
  showOriginal: boolean
  /** The thread's job is translating right now. */
  translating: boolean
}>()

const emit = defineEmits<{ toggleOriginal: []; openImage: [fileId: string] }>()

const { t } = useI18n()
const { toast } = useToast()

const isBot = computed(() => !!props.message.botId || !!props.user?.isBot)
const name = computed(() => props.user?.displayName || props.message.authorName || props.message.userId || '?')
const deleted = computed(() => props.message.subtype === 'tombstone')

const hasTranslation = computed(
  () => props.message.textVi != null && props.message.textVi !== props.message.textRaw,
)
/** The mrkdwn actually on screen. */
const shownText = computed(() =>
  props.showOriginal || props.message.textVi == null ? props.message.textRaw : props.message.textVi,
)
const bodyHtml = computed(() => renderSlackMrkdwn(shownText.value, props.ctx))

/** Tag shown when the Vietnamese text is missing. */
const untranslatedTag = computed(() => {
  const m = props.message
  if (m.textVi != null || m.translateStatus === 'skipped' || deleted.value) return null
  if (m.translateStatus === 'error') return { text: t('slackThread.message.translateFailed'), error: true }
  return props.translating
    ? { text: t('slackThread.message.translating'), error: false }
    : { text: t('slackThread.message.untranslated'), error: false }
})

const copied = ref(false)
async function copy() {
  try {
    await navigator.clipboard.writeText(mrkdwnToPlain(shownText.value, props.ctx))
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
    toast.success(t('slackThread.message.copied'))
  } catch {
    /* clipboard unavailable */
  }
}

// Links in the body must never navigate the webview: open them in the OS browser.
function onBodyClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (/^(https?:|mailto:)/i.test(href)) {
    openUrl(href).catch(() => {
      /* opener unavailable */
    })
  }
}
</script>

<template>
  <div
    class="group relative flex gap-2 px-5 transition-colors hover:bg-accent/30"
    :class="continued ? 'py-0.5' : 'pt-2 pb-1'"
  >
    <!-- Gutter: avatar, or the time on hover for a grouped message. -->
    <div class="w-9 shrink-0">
      <SlackAvatar
        v-if="!continued"
        :path="message.avatarPath"
        :name="name"
        :tone-key="message.userId ?? message.botId ?? name"
      />
      <span
        v-else
        class="block pt-0.5 text-right text-[10px] tabular-nums leading-5 text-muted-foreground opacity-0 group-hover:opacity-100"
        :title="formatFullDate(message.postedAt)"
      >
        {{ formatClock(message.postedAt) }}
      </span>
    </div>

    <div class="min-w-0 flex-1">
      <div v-if="!continued" class="flex items-baseline gap-1.5">
        <span class="truncate text-[14px] font-bold text-foreground" :title="user?.realName ?? undefined">
          {{ name }}
        </span>
        <span
          v-if="isBot"
          class="shrink-0 self-center rounded bg-muted px-1 py-px text-[9px] font-semibold text-muted-foreground"
        >
          {{ t('slackThread.message.app') }}
        </span>
        <span
          class="shrink-0 text-[11px] tabular-nums text-muted-foreground hover:underline"
          :title="formatFullDate(message.postedAt)"
        >
          {{ formatClock(message.postedAt) }}
        </span>
      </div>

      <p v-if="deleted" class="text-[14px] italic text-muted-foreground">
        {{ t('slackThread.message.deleted') }}
      </p>
      <template v-else>
        <!-- Sanitized by renderSlackMrkdwn (DOMPurify). -->
        <div
          v-if="shownText"
          class="slack-body whitespace-pre-wrap break-words text-[14px] leading-[1.46] text-foreground"
          @click="onBodyClick"
          v-html="bodyHtml"
        />
        <div
          v-if="message.editedTs || untranslatedTag"
          class="mt-0.5 flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground"
        >
          <span v-if="message.editedTs">{{ t('slackThread.message.edited') }}</span>
          <span
            v-if="untranslatedTag"
            class="rounded border px-1 py-px"
            :class="untranslatedTag.error ? 'border-destructive/40 text-destructive' : 'border-border'"
            :title="untranslatedTag.error ? message.translateError ?? undefined : undefined"
          >
            {{ untranslatedTag.text }}
          </span>
        </div>
      </template>

      <SlackMessageFiles :files="files" @open-image="emit('openImage', $event)" />

      <div v-if="message.reactions.length" class="mt-1 flex flex-wrap gap-1">
        <span
          v-for="r in message.reactions"
          :key="r.name"
          class="inline-flex items-center gap-1 rounded-full border border-border/70 bg-muted/40 px-1.5 py-px text-xs"
          :title="`:${r.name}:`"
        >
          <span>{{ emojiFor(r.name) ?? `:${r.name}:` }}</span>
          <span class="tabular-nums text-muted-foreground">{{ r.count }}</span>
        </span>
      </div>
    </div>

    <!-- Hover actions (this message only). -->
    <div
      v-if="!deleted"
      class="pointer-events-none absolute -top-3 right-4 flex items-center gap-0.5 rounded-md border border-border/60 bg-popover p-0.5 opacity-0 shadow-sm transition-opacity group-hover:pointer-events-auto group-hover:opacity-100 focus-within:pointer-events-auto focus-within:opacity-100"
    >
      <button
        v-if="hasTranslation"
        type="button"
        class="flex cursor-pointer items-center gap-1 rounded px-1.5 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        @click="emit('toggleOriginal')"
      >
        <Languages class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ showOriginal ? t('slackThread.message.showTranslation') : t('slackThread.message.showOriginal') }}
      </button>
      <button
        type="button"
        class="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        :title="t('slackThread.message.copy')"
        :aria-label="t('slackThread.message.copy')"
        @click="copy"
      >
        <component :is="copied ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
    </div>
  </div>
</template>
