<script setup lang="ts">
// Item counts of a settings backup about to be imported (file or LAN transfer).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { BACKUP_SECTION_KEYS, type BackupInfo } from '@/lib/settingsBackup'

const props = defineProps<{ info: BackupInfo }>()
const { t } = useI18n()

const rows = computed(() =>
  BACKUP_SECTION_KEYS
    .map(key => ({ key, count: props.info.counts[key] ?? 0 }))
    .filter(row => row.count > 0),
)
</script>

<template>
  <ul class="grid grid-cols-2 gap-x-4 gap-y-0.5 text-xs">
    <li v-for="row in rows" :key="row.key" class="flex justify-between gap-2">
      <span class="text-muted-foreground">{{ t(`settings.backup.sections.${row.key}`) }}</span>
      <span class="tabular-nums">{{ row.count }}</span>
    </li>
  </ul>
</template>
