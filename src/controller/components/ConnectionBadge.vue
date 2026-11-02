<script setup lang="ts">
/**
 * Connection state in the header's top-right corner, as one icon: spinner
 * while (re)connecting, Wifi when live, WifiOff otherwise — tinted by health.
 * The wording and the verified Host fingerprint live in its tooltip.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Wifi, WifiOff } from 'lucide-vue-next'
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
  connected.value && props.live ? 'text-success' : props.status === 'error' ? 'text-destructive' : 'text-warning',
)
const tooltip = computed(() =>
  props.hostFingerprint
    ? `${props.statusLabel} · ${t('controller.session.verifiedFingerprint', { fp: props.hostFingerprint })}`
    : props.statusLabel,
)
</script>

<template>
  <span
    role="status"
    class="inline-flex h-8 w-8 shrink-0 items-center justify-center"
    :class="tone"
    :title="tooltip"
    :aria-label="tooltip"
  >
    <Loader2 v-if="busy" class="h-4 w-4 animate-spin" :stroke-width="2" />
    <Wifi v-else-if="connected" class="h-4 w-4" :stroke-width="2" />
    <WifiOff v-else class="h-4 w-4" :stroke-width="2" />
  </span>
</template>
