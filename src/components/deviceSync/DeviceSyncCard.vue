<script setup lang="ts">
// Settings → Devices, "Sync between devices": when this machine last pulled, and the
// entry to the sync dialog. Pulling only needs a relay here (not Remote Control on).
import { computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowDownToLine } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { useLastSync } from '@/composables/useLastSync'

const emit = defineEmits<{ sync: [] }>()

const { t } = useI18n()
const store = useRemoteControlStore()
const { label, refresh } = useLastSync()
const hasRelay = computed(() => !!store.status?.relay_url?.trim() && !!store.status?.has_auth_token)
const last = computed(() => store.lastSync)

onMounted(refresh)
</script>

<template>
  <Card>
    <template #header>
      <ArrowDownToLine class="h-4 w-4 text-primary" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('deviceSync.card.title') }}</h3>
    </template>
    <div class="flex items-center gap-4 p-4">
      <div class="min-w-0 flex-1 space-y-1">
        <p v-if="label && last" class="text-xs">
          {{ label }}
          <span class="text-muted-foreground">· {{ t('deviceSync.last.counts', { n: last.categories, created: last.created, updated: last.updated }) }}</span>
        </p>
        <p v-else class="text-xs text-muted-foreground">{{ t('deviceSync.last.never') }}</p>
        <p class="text-[11px] text-muted-foreground">
          {{ hasRelay ? t('deviceSync.card.help') : t('deviceSync.card.needsRelay') }}
        </p>
      </div>
      <Button variant="outline" size="sm" :disabled="!hasRelay" @click="emit('sync')">
        <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('deviceSync.open') }}…
      </Button>
    </div>
  </Card>
</template>
