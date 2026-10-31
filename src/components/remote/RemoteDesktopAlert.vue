<script setup lang="ts">
/**
 * Pill at the bottom of a remote-desktop window, shown only while the link to
 * the controlled Devdy is down (after GRACE_MS, so a boot or a short blip does
 * not flash it) and gone again once it is back. The steady state lives in the
 * sidebar dot (RemoteDesktopIndicator.vue).
 */
import { onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RefreshCw } from 'lucide-vue-next'
import { useRemoteDesktopLink } from '@/composables/useRemoteDesktopLink'

const GRACE_MS = 3_000

const { t } = useI18n()
const link = useRemoteDesktopLink()
const { connected, label, stateText, dotClass, canReconnect, needsSignIn } = link

const shown = ref(false)
let timer: ReturnType<typeof setTimeout> | null = null
watch(
  connected,
  (ok) => {
    if (timer) clearTimeout(timer)
    timer = null
    if (ok) shown.value = false
    else if (link.enabled) timer = setTimeout(() => (shown.value = true), GRACE_MS)
  },
  { immediate: true },
)
onUnmounted(() => {
  if (timer) clearTimeout(timer)
})
</script>

<template>
  <div
    v-if="shown"
    class="fixed bottom-3 left-1/2 z-[1000] -translate-x-1/2 flex items-center gap-2 rounded-full border border-border bg-background/95 px-3 py-1.5 text-xs shadow-lg backdrop-blur"
    role="status"
  >
    <span class="h-2 w-2 rounded-full" :class="dotClass" />
    <span class="font-medium">{{ label }}</span>
    <span class="text-muted-foreground">{{ stateText }}</span>
    <button
      v-if="canReconnect && !needsSignIn"
      type="button"
      class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 hover:bg-muted"
      @click="link.reconnect"
    >
      <RefreshCw class="h-3 w-3" :stroke-width="2" /> {{ t('remote.desktop.banner.reconnect') }}
    </button>
    <button
      v-if="needsSignIn"
      type="button"
      class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-primary hover:bg-muted"
      @click="link.signOut"
    >
      {{ t('remote.desktop.banner.signInAgain') }}
    </button>
  </div>
</template>
