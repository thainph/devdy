<script setup lang="ts">
// Primary action of an inbox card (PR Inbox "AI review", Mentions "Check with
// AI"), icon-only; `title` carries the label. The button tracks the linked
// session: none → start (Sparkles), starting / running → spinner, done → open
// it (Eye). A dot in the run-status colour sits on its corner once a session
// exists (runDisplay mirrors StatusBadge's palette). An unread item takes
// that corner instead (the row itself carries no unread mark); a running
// session still shows through the spinner.
import { computed } from 'vue'
import { Button } from '@/components/ui'
import { runStatusDotClass } from '@/lib/runDisplay'
import { Eye, Loader2, Sparkles } from 'lucide-vue-next'

const props = withDefaults(defineProps<{
  state: 'none' | 'starting' | 'running' | 'done'
  title: string
  /** Status of the linked session, for the corner dot. */
  runStatus?: string | null
  compact?: boolean
  disabled?: boolean
  unread?: boolean
  /** Appended to the title when unread ("unread"). */
  unreadLabel?: string
}>(), { runStatus: null, compact: false, disabled: false, unread: false, unreadLabel: '' })

const emit = defineEmits<{ click: [] }>()

const hasSession = computed(() => props.state === 'running' || props.state === 'done')
const dotStatus = computed(() => (props.state === 'running' ? 'running' : props.runStatus))
const label = computed(() => (props.unread && props.unreadLabel ? `${props.title} · ${props.unreadLabel}` : props.title))
</script>

<template>
  <Button
    :variant="hasSession ? 'outline' : 'primary'"
    :size="compact ? 'icon-sm' : 'icon'"
    class="relative"
    :disabled="disabled || state === 'starting'"
    :title="label"
    :aria-label="label"
    @click="emit('click')"
  >
    <Loader2 v-if="state === 'starting' || state === 'running'" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
    <Eye v-else-if="state === 'done'" class="h-3.5 w-3.5" :stroke-width="1.75" />
    <Sparkles v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
    <!-- Unread: amber on the filled (start) button, primary on the outline one -->
    <span
      v-if="unread"
      class="absolute -right-1 -top-1 h-2.5 w-2.5 rounded-full ring-2 ring-card"
      :class="hasSession ? 'bg-primary' : 'bg-warning'"
    />
    <span
      v-else-if="hasSession && dotStatus"
      class="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full ring-2 ring-card"
      :class="[runStatusDotClass(dotStatus), { 'animate-pulse': state === 'running' }]"
    />
  </Button>
</template>
