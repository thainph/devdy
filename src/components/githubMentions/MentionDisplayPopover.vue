<script setup lang="ts">
// "Display" header button + popover for GitHub Mentions: how many days back
// to look, group by repo / person / nothing, and the muted threads / repos
// ("Not interested") with a way to unmute them, and the "Check with AI"
// prompt editor entry, and the background AI sorting switch. Same panel as PrInboxDisplayPopover.
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { onClickOutside } from '@vueuse/core'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { usePopoverFocus } from '@/composables/usePopoverFocus'
import { Button, ToggleSwitch } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { MENTION_GROUP_BYS, type MentionMute } from '@/lib/githubMentions'
import { BellOff, Check, ChevronRight, FolderX, SlidersHorizontal, Sparkles, X } from 'lucide-vue-next'

const emit = defineEmits<{ 'edit-prompt': [] }>()

const { t } = useI18n()
const { toast } = useToast()
const store = useGithubMentionsStore()
const LOOKBACK_CHOICES = [1, 3, 7, 14, 30]

function setLookback(days: number) {
  if (days === store.lookbackDays) return
  store.setLookbackDays(days).catch(e => toast.error(String(e)))
}
const open = ref(false)
const wrap = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)
onClickOutside(wrap, () => { open.value = false })
useEscapeClose(open)
usePopoverFocus(open, wrap, panel)
watch(open, o => { if (o) void store.loadMutes() })

function editPrompt() {
  open.value = false
  emit('edit-prompt')
}

function toggleAiTriage() {
  store.setAiTriage(!store.aiTriage).catch(e => toast.error(String(e)))
}

function unmute(m: MentionMute) {
  store.unmute(m)
    .then(() => toast.success(t('githubMentions.muted.unmuted', { name: m.label })))
    .catch(e => toast.error(String(e)))
}
</script>

<template>
  <div ref="wrap" class="relative shrink-0">
    <Button
      variant="outline"
      :class="open ? 'bg-accent text-foreground' : ''"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :title="t('githubMentions.display.button')"
      :aria-label="t('githubMentions.display.button')"
      @click="open = !open"
    >
      <SlidersHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
      <span class="hidden lg:inline">{{ t('githubMentions.display.button') }}</span>
    </Button>

    <div
      v-if="open"
      ref="panel"
      role="dialog"
      :aria-label="t('githubMentions.display.button')"
      class="absolute right-0 top-full z-50 mt-2 w-72 max-w-[calc(100vw-2rem)] rounded-lg border border-border/60 bg-popover p-3 shadow-xl shadow-black/30"
    >
      <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('githubMentions.display.lookback') }}</div>
      <div class="mb-3 flex flex-wrap gap-1" role="radiogroup" :aria-label="t('githubMentions.display.lookback')">
        <button
          v-for="d in LOOKBACK_CHOICES"
          :key="d"
          type="button"
          role="radio"
          :aria-checked="store.lookbackDays === d"
          class="rounded-md border px-2 py-1 text-xs tabular-nums cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          :class="store.lookbackDays === d ? 'border-primary bg-primary text-primary-foreground' : 'border-border text-muted-foreground hover:bg-accent/60 hover:text-foreground'"
          @click="setLookback(d)"
        >
          {{ t('githubMentions.display.days', d) }}
        </button>
      </div>

      <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('githubMentions.display.groupBy') }}</div>
      <div role="radiogroup" :aria-label="t('githubMentions.display.groupBy')">
        <button
          v-for="g in MENTION_GROUP_BYS"
          :key="g"
          type="button"
          role="radio"
          :aria-checked="store.groupBy === g"
          class="flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left text-xs cursor-pointer transition-colors hover:bg-accent/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          :class="store.groupBy === g ? 'text-foreground font-medium' : 'text-muted-foreground'"
          @click="store.setGroupBy(g)"
        >
          <span class="min-w-0 flex-1 truncate">{{ t(`githubMentions.display.${g}`) }}</span>
          <Check v-if="store.groupBy === g" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
        </button>
      </div>

      <!-- Background AI sorting into reply / FYI / done -->
      <div class="mt-3 flex items-start gap-2 px-1">
        <div class="min-w-0 flex-1">
          <p class="text-sm text-foreground">{{ t('githubMentions.display.aiTriage') }}</p>
          <p class="mt-0.5 text-[11px] text-muted-foreground">{{ t('githubMentions.display.aiTriageHint') }}</p>
        </div>
        <ToggleSwitch :checked="store.aiTriage" :label="t('githubMentions.display.aiTriage')" @toggle="toggleAiTriage" />
      </div>

      <!-- Prompt a "Check with AI" session starts with -->
      <button
        type="button"
        class="mt-3 flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer hover:bg-accent/60 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        @click="editPrompt"
      >
        <Sparkles class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="min-w-0 flex-1 text-sm text-foreground truncate">{{ t('githubMentions.prompt.button') }}</span>
        <ChevronRight class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
      </button>

      <div class="mb-1.5 mt-3 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
        {{ t('githubMentions.muted.title') }}
      </div>
      <p v-if="store.mutes.length === 0" class="text-xs text-muted-foreground">{{ t('githubMentions.muted.empty') }}</p>
      <ul v-else class="flex max-h-48 flex-col overflow-auto">
        <li v-for="m in store.mutes" :key="`${m.scope}:${m.target}`" class="flex min-w-0 items-center gap-2 rounded-md px-1.5 py-1 hover:bg-accent/60">
          <component
            :is="m.scope === 'repo' ? FolderX : BellOff"
            class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
            :stroke-width="1.75"
            :title="t(`githubMentions.muted.${m.scope}`)"
          />
          <span class="min-w-0 flex-1 truncate text-xs" :class="{ 'font-mono': m.scope === 'repo' }" :title="m.label">{{ m.label }}</span>
          <button
            type="button"
            class="shrink-0 rounded p-0.5 cursor-pointer text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            :title="t('githubMentions.muted.unmute')"
            :aria-label="t('githubMentions.muted.unmute')"
            @click="unmute(m)"
          >
            <X class="h-3 w-3" :stroke-width="2" />
          </button>
        </li>
      </ul>
    </div>
  </div>
</template>
