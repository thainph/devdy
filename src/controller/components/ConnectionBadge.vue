<script setup lang="ts">
/**
 * Connection state next to a tab title: spinner / Wifi / WifiOff with a short
 * label, plus a shield once the Host's fingerprint is verified (the full
 * fingerprint is in its tooltip).
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, ShieldCheck, Wifi, WifiOff } from 'lucide-vue-next'
import type { ConnStatus } from '../connection'

const props = defineProps<{
  status: ConnStatus
  live: boolean
  statusLabel: string
  hostFingerprint: string | null
}>()

const { t } = useI18n()

const connected = computed(() => props.status === 'connected')
const busy = computed(() => ['joining', 'handshaking', 'reconnecting'].includes(props.status))
const tone = computed(() =>
  connected.value && props.live ? 'text-emerald-500' : props.status === 'error' ? 'text-destructive' : 'text-amber-500',
)
</script>

<template>
  <span class="inline-flex min-w-0 items-center gap-1 text-xs font-medium" :class="tone">
    <Loader2 v-if="busy" class="h-3.5 w-3.5 shrink-0 animate-spin" :stroke-width="2" />
    <Wifi v-else-if="connected" class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
    <WifiOff v-else class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
    <span class="truncate">{{ statusLabel }}</span>
    <span
      v-if="hostFingerprint"
      class="inline-flex shrink-0 text-emerald-500"
      :title="t('controller.session.verifiedFingerprint', { fp: hostFingerprint })"
    >
      <ShieldCheck class="h-3.5 w-3.5" :stroke-width="2" />
    </span>
  </span>
</template>
