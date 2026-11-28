<script setup lang="ts">
// Display sections of the PR inbox's settings modal (InboxOptionsButton holds
// the button and the modal), in RadarSettingsModal's style: group-by (a
// bordered radio list), density, the background check interval (pr_inbox
// poller — new-PR notifications), bots (hide switch + the bot-accounts
// dialog) and the review / re-review prompt editors as bordered "→" entries.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useToast } from '@/composables/useToast'
import { usePrInboxStore, PR_POLL_MINUTES_OPTIONS, type PrInboxDensity, type PrPromptKind } from '@/stores/prInbox'
import { PR_GROUP_BY_OPTIONS, type PrGroupBy } from '@/lib/prInboxGrouping'
import { SegmentedControl, ToggleSwitch } from '@/components/ui'
import { Check, KeyRound, ChevronRight, RotateCcw, Sparkles } from 'lucide-vue-next'

const PROMPT_ITEMS: { kind: PrPromptKind; icon: typeof Sparkles }[] = [
  { kind: 'review', icon: Sparkles },
  { kind: 'rereview', icon: RotateCcw },
]

const props = defineProps<{
  /** Shuts the settings modal (before an entry opens another dialog). */
  close: () => void
}>()

const emit = defineEmits<{ 'manage-bots': []; 'edit-prompt': [kind: PrPromptKind] }>()

const { t } = useI18n()
const store = usePrInboxStore()
const { toast } = useToast()

const radiogroup = ref<HTMLElement | null>(null)

// Group-by radio group: roving tabindex (only the checked radio is tabbable);
// arrow keys move the selection and focus, wrapping around, like native radios.
function onRadioKey(e: KeyboardEvent, current: PrGroupBy) {
  const step = e.key === 'ArrowDown' || e.key === 'ArrowRight' ? 1
    : e.key === 'ArrowUp' || e.key === 'ArrowLeft' ? -1
      : 0
  const n = PR_GROUP_BY_OPTIONS.length
  let next: number
  if (step !== 0) next = (PR_GROUP_BY_OPTIONS.indexOf(current) + step + n) % n
  else if (e.key === 'Home') next = 0
  else if (e.key === 'End') next = n - 1
  else return
  e.preventDefault()
  store.setGroupBy(PR_GROUP_BY_OPTIONS[next])
  radiogroup.value?.querySelectorAll<HTMLElement>('[role="radio"]')[next]?.focus()
}

const DENSITIES: readonly PrInboxDensity[] = ['comfortable', 'compact']
const densityOptions = computed(() => DENSITIES.map(d => ({ value: d, label: t(`prInbox.density.${d}`) })))

// Read on open (the panel mounts then): the poller interval is a backend
// setting another window may change.
onMounted(() => { void store.loadPollMinutes() })
const pollOptions = computed(() => PR_POLL_MINUTES_OPTIONS.map(n => ({
  value: n as number,
  label: n === 0 ? t('prInbox.poll.off') : t('prInbox.poll.minutes', { n }),
})))

async function setPoll(minutes: number) {
  try {
    await store.setPollMinutes(minutes)
  } catch (e) {
    toast.error(t('prInbox.poll.saveFailed', { error: String(e) }))
  }
}

function manageBots() {
  props.close()
  emit('manage-bots')
}

function editPrompt(kind: PrPromptKind) {
  props.close()
  emit('edit-prompt', kind)
}
</script>

<template>
  <!-- Group by -->
  <section class="space-y-2">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.groupByLabel') }}</h3>
    <div
      ref="radiogroup"
      role="radiogroup"
      :aria-label="t('prInbox.groupByLabel')"
      class="overflow-hidden rounded-md border border-border/70"
    >
      <button
        v-for="g in PR_GROUP_BY_OPTIONS"
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
        <span class="min-w-0 flex-1 truncate">{{ t(`prInbox.groupBy.${g}`) }}</span>
        <Check v-if="store.groupBy === g" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
      </button>
    </div>
  </section>

  <!-- Density · background check -->
  <section class="space-y-3">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.display.section') }}</h3>
    <div class="flex items-center gap-3">
      <div class="min-w-0 flex-1 text-[13px]">{{ t('prInbox.density.label') }}</div>
      <SegmentedControl
        :model-value="store.density"
        :options="densityOptions"
        :stretch="false"
        :aria-label="t('prInbox.density.label')"
        @update:model-value="store.setDensity"
      />
    </div>
    <div class="space-y-1.5">
      <div>
        <div class="text-[13px]">{{ t('prInbox.poll.label') }}</div>
        <div class="text-[11px] text-muted-foreground">{{ t('prInbox.poll.hint') }}</div>
      </div>
      <SegmentedControl
        :model-value="store.pollMinutes ?? -1"
        :options="pollOptions"
        :aria-label="t('prInbox.poll.label')"
        @update:model-value="setPoll"
      />
    </div>
  </section>

  <!-- Bots -->
  <section class="space-y-3">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.botsGroup') }}</h3>
    <div class="flex items-center gap-3">
      <div class="min-w-0 flex-1">
        <div class="text-[13px]">{{ t('prInbox.hideBots') }}</div>
        <div class="text-[11px] text-muted-foreground">{{ t('prInbox.display.hideBotsHint') }}</div>
      </div>
      <ToggleSwitch
        size="md"
        :checked="store.hideBots"
        :label="t('prInbox.hideBots')"
        @toggle="store.setHideBots(!store.hideBots)"
      />
    </div>
    <button
      type="button"
      class="flex w-full items-center gap-3 rounded-md border border-border/70 px-3 py-2.5 text-left transition-colors hover:bg-accent"
      @click="manageBots"
    >
      <KeyRound class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
      <span class="min-w-0 flex-1 truncate text-[13px]">{{ t('prInbox.botAccounts.button') }}</span>
      <span v-if="store.botLogins.length > 0" class="shrink-0 text-[11px] tabular-nums text-muted-foreground">{{ store.botLogins.length }}</span>
      <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
    </button>
  </section>

  <!-- Prompts a review starts with / "Re-review" sends to the same session -->
  <section class="space-y-2">
    <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.prompt.section') }}</h3>
    <button
      v-for="p in PROMPT_ITEMS"
      :key="p.kind"
      type="button"
      class="flex w-full items-center gap-3 rounded-md border border-border/70 px-3 py-2.5 text-left transition-colors hover:bg-accent"
      @click="editPrompt(p.kind)"
    >
      <component :is="p.icon" class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
      <span class="min-w-0 flex-1 truncate text-[13px]">{{ t(`prInbox.prompt.${p.kind}.button`) }}</span>
      <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
    </button>
  </section>
</template>
