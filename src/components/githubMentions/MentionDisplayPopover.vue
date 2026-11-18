<script setup lang="ts">
// "Display" header button + popover for GitHub Mentions: group by repo /
// person / nothing. Same panel as PrInboxDisplayPopover.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { onClickOutside } from '@vueuse/core'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { usePopoverFocus } from '@/composables/usePopoverFocus'
import { Button } from '@/components/ui'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { MENTION_GROUP_BYS } from '@/lib/githubMentions'
import { Check, SlidersHorizontal } from 'lucide-vue-next'

const { t } = useI18n()
const store = useGithubMentionsStore()
const open = ref(false)
const wrap = ref<HTMLElement | null>(null)
const panel = ref<HTMLElement | null>(null)
onClickOutside(wrap, () => { open.value = false })
useEscapeClose(open)
usePopoverFocus(open, wrap, panel)
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
      class="absolute right-0 top-full z-50 mt-2 w-56 max-w-[calc(100vw-2rem)] rounded-lg border border-border/60 bg-popover p-3 shadow-xl shadow-black/30"
    >
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
    </div>
  </div>
</template>
