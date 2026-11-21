<script setup lang="ts">
// Title-bar "waiting for you" pill: how many runs are blocked on a permission /
// question request. Self-contained (reads the shared permission state), renders
// nothing when no run is asking. Click → bring the first asking run's card to the
// front of the in-app stack and focus it; when no in-app stack is showing (Devdy
// not in front, or no stack mounted), open that run's session instead.
//
// Props:
//   - `showLabel` (default true): the "waiting" text after the count; pass
//     `:show-label="false"` for a count-only pill in tight spots.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ShieldAlert } from 'lucide-vue-next'
import { stackLayout } from '@/lib/permissionSurface'
import { IS_SESSION_WINDOW } from '@/lib/windowMode'
import { usePermissionDecisions } from '@/composables/usePermissionDecisions'

const props = withDefaults(defineProps<{ showLabel?: boolean }>(), { showLabel: true })

const { t } = useI18n()
const perms = usePermissionDecisions()

// Only the main window owns the permission flow (a pop-out sees a single run).
const count = computed(() => (IS_SESSION_WINDOW ? 0 : perms.entries.value.length))

// "First" = what the stack shows first: its front card, else the first hidden one.
const firstRunId = computed(() => {
  const l = stackLayout(perms.entries.value, perms.collapsed.value, perms.promoted.value)
  return (l.visible[0] ?? l.overflow[0] ?? l.collapsed[0])?.runId ?? null
})

const title = computed(() => t('permission.pill.title', { n: count.value }))

function onClick() {
  if (firstRunId.value) perms.focusRun(firstRunId.value)
}
</script>

<template>
  <button
    v-if="count > 0"
    type="button"
    class="inline-flex h-6 shrink-0 items-center gap-1 rounded-full border border-warning/30 bg-warning/15 px-2 text-[10px] font-medium text-warning transition-colors hover:bg-warning/25 cursor-pointer"
    :title="title"
    :aria-label="title"
    @click="onClick"
  >
    <ShieldAlert class="h-3 w-3" :stroke-width="2" />
    <span class="tabular-nums">{{ count }}</span>
    <span v-if="props.showLabel">{{ t('permission.pill.label') }}</span>
  </button>
</template>
