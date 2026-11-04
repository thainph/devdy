<script setup lang="ts">
// Entry point of the device pull sync dialog. `compact`: the bare icon (sidebar footer);
// otherwise an `xs` outline button with a label (Settings card headers).
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RefreshCw } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import DeviceSyncDialog from './DeviceSyncDialog.vue'

defineProps<{ compact?: boolean }>()

const { t } = useI18n()
const open = ref(false)
</script>

<template>
  <span class="inline-flex">
    <Button
      v-if="compact"
      variant="ghost"
      size="icon-xs"
      :title="t('deviceSync.open')"
      :aria-label="t('deviceSync.open')"
      @click="open = true"
    >
      <RefreshCw class="h-3 w-3" :stroke-width="1.75" />
    </Button>
    <Button v-else variant="outline" size="xs" :title="t('deviceSync.open')" @click="open = true">
      <RefreshCw class="h-3 w-3" :stroke-width="1.75" />
      {{ t('deviceSync.button') }}
    </Button>
    <!-- To <body>: Card headers use backdrop-filter, which would trap a fixed overlay. -->
    <Teleport to="body">
      <DeviceSyncDialog v-if="open" :open="open" @close="open = false" />
    </Teleport>
  </span>
</template>
