<script setup lang="ts">
// What spawned a tracked process entry: a session (user work, primary tone) or
// one of Devdy's own helpers (neutral). Shared by the Processes screen and the
// startup / quit prompts so a kind reads the same everywhere.
import { useI18n } from 'vue-i18n'
import { Badge } from '@/components/ui'
import type { ProcKind } from '@/lib/backgroundProcesses'

defineProps<{ kind: ProcKind }>()
const { t } = useI18n()
</script>

<template>
  <Badge :tone="kind === 'run' ? 'primary' : 'neutral'" size="xs" class="shrink-0">
    {{ t(`processes.kind.${kind}`) }}
  </Badge>
</template>
