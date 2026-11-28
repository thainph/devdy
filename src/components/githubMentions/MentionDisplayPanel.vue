<script setup lang="ts">
// Display settings of the GitHub Mentions settings modal (MentionOptionsMenu),
// styled like Delivery Radar's: group by project / repo / person / nothing (a
// bordered radio list, like the PR Inbox's), how many days back to look, the
// background AI sorting switch, the entry to the
// "Check with AI" prompt editor, and the muted threads / repos ("Not
// interested") with a way to unmute them.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, SegmentedControl, ToggleSwitch } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { MENTION_GROUP_BYS, type MentionGroupBy, type MentionMute } from '@/lib/githubMentions'
import { BellOff, Check, ChevronRight, FolderX, Sparkles, X } from 'lucide-vue-next'

/** The parent closes the modal, then opens the prompt editor. */
const emit = defineEmits<{ 'edit-prompt': [] }>()

const { t } = useI18n()
const { toast } = useToast()
const store = useGithubMentionsStore()
const LOOKBACK_CHOICES = [1, 3, 7, 14, 30]

const lookbackOptions = computed(() => LOOKBACK_CHOICES.map(d => ({ value: d, label: t('githubMentions.display.days', d) })))

const radiogroup = ref<HTMLElement | null>(null)

// Group-by radio group: roving tabindex (only the checked radio is tabbable);
// arrow keys move the selection and focus, wrapping around, like native radios.
function onRadioKey(e: KeyboardEvent, current: MentionGroupBy) {
  const step = e.key === 'ArrowDown' || e.key === 'ArrowRight' ? 1
    : e.key === 'ArrowUp' || e.key === 'ArrowLeft' ? -1
      : 0
  const n = MENTION_GROUP_BYS.length
  let next: number
  if (step !== 0) next = (MENTION_GROUP_BYS.indexOf(current) + step + n) % n
  else if (e.key === 'Home') next = 0
  else if (e.key === 'End') next = n - 1
  else return
  e.preventDefault()
  store.setGroupBy(MENTION_GROUP_BYS[next])
  radiogroup.value?.querySelectorAll<HTMLElement>('[role="radio"]')[next]?.focus()
}

function setLookback(days: number) {
  if (days === store.lookbackDays) return
  store.setLookbackDays(days).catch(e => toast.error(String(e)))
}

// Mounted each time the modal opens.
onMounted(() => { void store.loadMutes() })

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
  <!-- Group by -->
  <section class="space-y-2">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('githubMentions.display.groupBy') }}</h3>
    <div
      ref="radiogroup"
      role="radiogroup"
      :aria-label="t('githubMentions.display.groupBy')"
      class="overflow-hidden rounded-md border border-border/70"
    >
      <button
        v-for="g in MENTION_GROUP_BYS"
        :key="g"
        type="button"
        role="radio"
        :aria-checked="store.groupBy === g"
        :tabindex="store.groupBy === g ? 0 : -1"
        class="flex w-full cursor-pointer items-center gap-2.5 border-b border-border/40 px-3 py-1.5 text-left text-[13px] transition-colors last:border-b-0 hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
        :class="store.groupBy === g ? 'font-medium text-foreground' : 'text-muted-foreground'"
        @click="store.setGroupBy(g)"
        @keydown="onRadioKey($event, g)"
      >
        <span class="min-w-0 flex-1 truncate">{{ t(`githubMentions.display.${g}`) }}</span>
        <Check v-if="store.groupBy === g" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
      </button>
    </div>
  </section>

  <section class="space-y-3">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('githubMentions.display.title') }}</h3>

    <div class="space-y-1.5">
      <div>
        <div class="text-[13px]">{{ t('githubMentions.display.lookback') }}</div>
        <div class="text-[11px] text-muted-foreground">{{ t('githubMentions.display.lookbackHint') }}</div>
      </div>
      <SegmentedControl
        :model-value="store.lookbackDays"
        :options="lookbackOptions"
        :aria-label="t('githubMentions.display.lookback')"
        @update:model-value="setLookback"
      />
    </div>

    <!-- Background AI sorting into reply / FYI / done -->
    <div class="flex items-center gap-3">
      <div class="min-w-0 flex-1">
        <div class="text-[13px]">{{ t('githubMentions.display.aiTriage') }}</div>
        <div class="text-[11px] text-muted-foreground">{{ t('githubMentions.display.aiTriageHint') }}</div>
      </div>
      <ToggleSwitch size="md" :checked="store.aiTriage" :label="t('githubMentions.display.aiTriage')" @toggle="toggleAiTriage" />
    </div>
  </section>

  <!-- Prompt a "Check with AI" session starts with -->
  <section class="space-y-2">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('githubMentions.check.label') }}</h3>
    <button
      type="button"
      class="flex w-full cursor-pointer items-center gap-3 rounded-md border border-border/70 px-3 py-2.5 text-left transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      @click="emit('edit-prompt')"
    >
      <Sparkles class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
      <span class="min-w-0 flex-1">
        <span class="block text-[13px]">{{ t('githubMentions.prompt.button') }}</span>
        <span class="block text-[11px] text-muted-foreground">{{ t('githubMentions.prompt.buttonHint') }}</span>
      </span>
      <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
    </button>
  </section>

  <section class="space-y-2">
    <h3 class="flex items-center text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
      {{ t('githubMentions.muted.title') }}
      <span v-if="store.mutes.length > 0" class="ml-auto font-normal normal-case tracking-normal tabular-nums">{{ store.mutes.length }}</span>
    </h3>
    <p v-if="store.mutes.length === 0" class="text-[11px] text-muted-foreground">{{ t('githubMentions.muted.empty') }}</p>
    <ul v-else class="max-h-48 overflow-auto rounded-md border border-border/70">
      <li
        v-for="m in store.mutes"
        :key="`${m.scope}:${m.target}`"
        class="flex min-w-0 items-center gap-2.5 border-b border-border/40 px-3 py-1.5 last:border-b-0"
      >
        <component
          :is="m.scope === 'repo' ? FolderX : BellOff"
          class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
          :stroke-width="1.75"
          :title="t(`githubMentions.muted.${m.scope}`)"
        />
        <span class="min-w-0 flex-1 truncate text-[13px]" :class="{ 'font-mono': m.scope === 'repo' }" :title="m.label">{{ m.label }}</span>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :title="t('githubMentions.muted.unmute')"
          :aria-label="t('githubMentions.muted.unmute')"
          @click="unmute(m)"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </Button>
      </li>
    </ul>
  </section>
</template>
