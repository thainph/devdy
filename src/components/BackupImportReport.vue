<script setup lang="ts">
// Result of a settings import (file or LAN transfer): per-section counts, secrets, warnings.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, RotateCcw } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { BACKUP_SECTION_KEYS, type ImportCounts, type ImportReport } from '@/lib/settingsBackup'

const props = defineProps<{ report: ImportReport }>()
const { t } = useI18n()

const rows = computed(() =>
  BACKUP_SECTION_KEYS
    .map(key => ({ key, c: props.report.counts[key] }))
    .filter((row): row is { key: (typeof BACKUP_SECTION_KEYS)[number]; c: ImportCounts } => !!row.c),
)

// Settings, language and every list store were loaded before the import: reload to pick it all up.
function reloadApp() {
  window.location.reload()
}
</script>

<template>
  <div class="space-y-2 rounded-lg border border-border/60 p-3">
    <p class="text-xs font-medium">{{ t('settings.backup.report.title') }}</p>
    <table class="w-full text-xs">
      <thead class="text-[10px] uppercase tracking-wider text-muted-foreground">
        <tr>
          <th class="text-left font-medium" />
          <th class="text-right font-medium">{{ t('settings.backup.report.created') }}</th>
          <th class="text-right font-medium">{{ t('settings.backup.report.updated') }}</th>
          <th class="text-right font-medium">{{ t('settings.backup.report.unchanged') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="row in rows" :key="row.key">
          <td class="text-muted-foreground">{{ t(`settings.backup.sections.${row.key}`) }}</td>
          <td class="text-right tabular-nums">{{ row.c.created }}</td>
          <td class="text-right tabular-nums">{{ row.c.updated }}</td>
          <td class="text-right tabular-nums text-muted-foreground">{{ row.c.unchanged }}</td>
        </tr>
      </tbody>
    </table>
    <p class="text-[11px] text-muted-foreground">
      {{ report.secretsImported ? t('settings.backup.report.secretsImported') : t('settings.backup.report.secretsNotImported') }}
    </p>
    <p class="text-[11px] text-muted-foreground">{{ t('settings.backup.report.reloginHint') }}</p>
    <ul v-if="report.warnings.length" class="space-y-1 text-[11px] text-amber-600 dark:text-amber-400">
      <li v-for="(w, i) in report.warnings" :key="i" class="flex items-start gap-1.5">
        <AlertTriangle class="mt-px h-3 w-3 shrink-0" :stroke-width="1.75" />
        <span class="break-all">{{ w }}</span>
      </li>
    </ul>
    <Button variant="outline" size="xs" @click="reloadApp">
      <RotateCcw class="h-3 w-3" :stroke-width="1.75" />
      {{ t('settings.backup.report.reload') }}
    </Button>
  </div>
</template>
