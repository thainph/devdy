<script setup lang="ts">
// Icon of the "now" line: what kind of step the session is in.
import { computed } from 'vue'
import {
  Bot, Eye, Globe, Hourglass, MessageCircleQuestion, Pencil, Plug, Search, ShieldAlert, Sparkles, SquareTerminal,
} from 'lucide-vue-next'
import type { NowLine } from '@/lib/runtimeDashboard'

const props = defineProps<{ now: NowLine }>()

const icon = computed(() => {
  const n = props.now
  if (n.kind === 'permission') return ShieldAlert
  if (n.kind === 'question') return MessageCircleQuestion
  if (n.kind === 'wait') return Hourglass
  if ((n.kind !== 'tool' && n.kind !== 'last') || !n.tool) return Sparkles
  const name = n.tool.toLowerCase()
  if (name.startsWith('mcp__')) return Plug
  if (name === 'bash' || name === 'bashoutput') return SquareTerminal
  if (['edit', 'write', 'multiedit', 'notebookedit'].includes(name)) return Pencil
  if (name === 'read') return Eye
  if (name === 'grep' || name === 'glob' || name === 'toolsearch') return Search
  if (name === 'webfetch' || name === 'websearch') return Globe
  if (name === 'task' || name === 'agent') return Bot
  return Sparkles
})
</script>

<template>
  <component :is="icon" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" aria-hidden="true" />
</template>
