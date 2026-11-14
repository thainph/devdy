<script setup lang="ts">
// "Display" toolbar button + popover for the PR inbox: group-by, density,
// hide-bots and the bot-accounts dialog. Mirrors CalendarView's display-options
// popover — same trigger state, panel, uppercase section labels, segmented
// control and full-row `role="switch"` toggle — so view settings look the same
// on every screen. Group-by is a radio list rather than an AppSelect: the
// select's teleported menu sits outside the panel, so picking an option would
// count as an outside click and close the popover. Also holds the background
// check interval (pr_inbox poller — new-PR notifications).
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { onClickOutside } from '@vueuse/core'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { usePopoverFocus } from '@/composables/usePopoverFocus'
import { useToast } from '@/composables/useToast'
import { usePrInboxStore, PR_POLL_MINUTES_OPTIONS, type PrInboxDensity } from '@/stores/prInbox'
import { PR_GROUP_BY_OPTIONS, type PrGroupBy } from '@/lib/prInboxGrouping'
import { Button, SegmentedControl } from '@/components/ui'
import { Check, EyeOff, KeyRound, SlidersHorizontal, ChevronRight, RotateCcw } from 'lucide-vue-next'

const emit = defineEmits<{ 'manage-bots': []; 'edit-rereview-prompt': [] }>()

const { t } = useI18n()
const store = usePrInboxStore()
const { toast } = useToast()

const open = ref(false)
const wrap = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)
const radiogroup = ref<HTMLElement | null>(null)
onClickOutside(wrap, () => { open.value = false })
useEscapeClose(open)
usePopoverFocus(open, wrap, panel)

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

// Read on open: the poller interval is a backend setting another window may change.
watch(open, (o) => { if (o) void store.loadPollMinutes() })
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
  open.value = false
  emit('manage-bots')
}

function editReReviewPrompt() {
  open.value = false
  emit('edit-rereview-prompt')
}
</script>

<template>
  <div ref="wrap" class="relative shrink-0">
    <Button
      variant="outline"
      :class="open ? 'bg-accent text-foreground' : ''"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :title="t('prInbox.display.button')"
      :aria-label="t('prInbox.display.button')"
      @click="open = !open"
    >
      <SlidersHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
      <span class="hidden @[30rem]/toolbar:inline">{{ t('prInbox.display.button') }}</span>
    </Button>

    <div
      v-if="open"
      ref="panel"
      role="dialog"
      :aria-label="t('prInbox.display.button')"
      class="absolute right-0 top-full z-50 mt-2 max-h-[70vh] w-64 max-w-[calc(100vw-2rem)] overflow-y-auto rounded-lg border border-border/60 bg-popover p-3 shadow-xl shadow-black/30"
    >
      <!-- Group by -->
      <div class="mb-3">
        <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.groupByLabel') }}</div>
        <div ref="radiogroup" role="radiogroup" :aria-label="t('prInbox.groupByLabel')">
          <button
            v-for="g in PR_GROUP_BY_OPTIONS"
            :key="g"
            type="button"
            role="radio"
            :aria-checked="store.groupBy === g"
            :tabindex="store.groupBy === g ? 0 : -1"
            class="flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left text-xs cursor-pointer transition-colors hover:bg-accent/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            :class="store.groupBy === g ? 'text-foreground font-medium' : 'text-muted-foreground'"
            @click="store.setGroupBy(g)"
            @keydown="onRadioKey($event, g)"
          >
            <span class="min-w-0 flex-1 truncate">{{ t(`prInbox.groupBy.${g}`) }}</span>
            <Check v-if="store.groupBy === g" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
          </button>
        </div>
      </div>

      <!-- Density -->
      <div class="mb-3">
        <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.density.label') }}</div>
        <SegmentedControl
          :model-value="store.density"
          :options="densityOptions"
          :aria-label="t('prInbox.density.label')"
          @update:model-value="store.setDensity"
        />
      </div>

      <!-- Background check + new-PR notifications -->
      <div class="mb-3">
        <div class="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('prInbox.poll.label') }}</div>
        <SegmentedControl
          :model-value="store.pollMinutes ?? -1"
          :options="pollOptions"
          :aria-label="t('prInbox.poll.label')"
          @update:model-value="setPoll"
        />
        <div class="mt-1 text-[11px] text-muted-foreground">{{ t('prInbox.poll.hint') }}</div>
      </div>

      <!-- Prompt "Re-review" sends to the existing review session -->
      <div class="mb-3">
        <button
          type="button"
          class="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer hover:bg-accent/60 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          @click="editReReviewPrompt"
        >
          <RotateCcw class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="min-w-0 flex-1 text-sm text-foreground truncate">{{ t('prInbox.reReviewPrompt.button') }}</span>
          <ChevronRight class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        </button>
      </div>

      <!-- Bots -->
      <div class="pt-3 border-t border-border/60">
        <button
          type="button"
          role="switch"
          :aria-checked="store.hideBots"
          class="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          @click="store.setHideBots(!store.hideBots)"
        >
          <EyeOff class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="min-w-0 flex-1">
            <span class="block text-sm text-foreground">{{ t('prInbox.hideBots') }}</span>
            <span class="block text-[11px] text-muted-foreground truncate">{{ t('prInbox.display.hideBotsHint') }}</span>
          </span>
          <span
            class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
            :class="store.hideBots ? 'bg-primary' : 'bg-muted'"
          >
            <span
              class="inline-block h-4 w-4 rounded-full bg-white shadow transition-transform"
              :class="store.hideBots ? 'translate-x-4' : 'translate-x-0.5'"
            />
          </span>
        </button>
        <button
          type="button"
          class="mt-1 flex w-full items-center gap-2 rounded-md px-1 py-1 text-left cursor-pointer hover:bg-accent/60 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          @click="manageBots"
        >
          <KeyRound class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="min-w-0 flex-1 text-sm text-foreground truncate">{{ t('prInbox.botAccounts.button') }}</span>
          <span v-if="store.botLogins.length > 0" class="text-[11px] text-muted-foreground tabular-nums">{{ store.botLogins.length }}</span>
          <ChevronRight class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        </button>
      </div>
    </div>
  </div>
</template>
