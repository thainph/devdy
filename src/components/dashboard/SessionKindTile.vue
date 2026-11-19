<script setup lang="ts">
// A session's kind as a tinted tile with an icon — plain session, PR review,
// issue analysis, conductor, or a CLI session outside Devdy. Shared by the
// Dashboard's running rows, its History and the project's session list, so a
// kind reads the same everywhere.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { CircleDot, GitPullRequest, MessageSquare, Network, SquareTerminal } from 'lucide-vue-next'
import type { SessionKind } from '@/lib/runtimeDashboard'

const props = withDefaults(defineProps<{ kind: SessionKind; size?: 'xs' | 'sm' | 'md' | 'lg' }>(), { size: 'lg' })
const { t } = useI18n()

const KIND = {
  session: { icon: MessageSquare, tile: 'bg-primary/12 text-primary' },
  pr: { icon: GitPullRequest, tile: 'bg-info/14 text-info' },
  issue: { icon: CircleDot, tile: 'bg-success/13 text-success' },
  conductor: { icon: Network, tile: 'bg-accent text-accent-foreground' },
  external: { icon: SquareTerminal, tile: 'bg-muted text-muted-foreground' },
} as const

const k = computed(() => KIND[props.kind])
/** Tile and icon per size: 18 / 22 / 26 / 30 px. */
const SIZE = {
  xs: { box: 'h-[18px] w-[18px] rounded-[5px]', icon: 'h-2.5 w-2.5' },
  sm: { box: 'h-[22px] w-[22px] rounded-md', icon: 'h-3 w-3' },
  md: { box: 'h-[26px] w-[26px] rounded-[7px]', icon: 'h-[13px] w-[13px]' },
  lg: { box: 'h-[30px] w-[30px] rounded-lg', icon: 'h-3.5 w-3.5' },
} as const
</script>

<template>
  <span
    class="grid shrink-0 place-items-center"
    :class="[k.tile, SIZE[size].box]"
    :title="t(`dashboard.kind.${kind}`)"
  >
    <component :is="k.icon" :class="SIZE[size].icon" :stroke-width="2" aria-hidden="true" />
    <span class="sr-only">{{ t(`dashboard.kind.${kind}`) }}</span>
  </span>
</template>
