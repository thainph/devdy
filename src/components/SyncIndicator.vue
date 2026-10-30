<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useSyncStore } from '@/stores/sync'
import { SYNC_DOT_CLASS, ageParts, syncErrorCode, syncStateKey, syncTone } from '@/lib/syncCore'
import { isRemoteMode } from '@/lib/ipc/remote'

/**
 * Sidebar-footer dot for two-way sync: colour = state, tooltip = state + last pull + pending +
 * refused (parked) changes + the last error.
 * Hidden while this device is not in a sync group and in remote-desktop windows (it would show
 * the other machine's state). Click opens Settings → Sync.
 */

const { t } = useI18n()
const router = useRouter()
const store = useSyncStore()
const enabled = !isRemoteMode()

const status = computed(() => store.status)
const tone = computed(() => syncTone(status.value, store.mergePending))

const tooltip = computed(() => {
  const s = status.value
  const { unit, n } = ageParts(s.last_pull)
  const parts = [
    `${t('settings.sync.title')}: ${t(`settings.sync.state.${syncStateKey(s, store.mergePending)}`)}`,
    `${t('settings.sync.status.lastPull')}: ${t(`settings.sync.ago.${unit}`, { n })}`,
  ]
  if (s.pending) parts.push(`${t('settings.sync.status.pending')}: ${s.pending}`)
  if (s.parked) parts.push(t('settings.sync.parked.tooltip', { n: s.parked }))
  if (s.last_error && s.state !== 'online') {
    const code = syncErrorCode(s.last_error)
    parts.push(code ? t(`settings.sync.errors.${code}`) : s.last_error)
  }
  return parts.join('\n')
})

function openSettings() {
  router.push({ path: '/settings', query: { section: 'sync' } }).catch(() => {})
}

onMounted(() => {
  if (enabled) store.start().catch(() => {})
})
</script>

<template>
  <button
    v-if="enabled && (status.state !== 'disabled' || store.mergePending)"
    type="button"
    class="flex items-center gap-1.5 rounded px-1 text-[10px] text-muted-foreground/70 hover:text-foreground"
    :title="tooltip"
    :aria-label="tooltip"
    @click="openSettings"
  >
    <span class="relative flex h-2 w-2">
      <span
        v-if="tone === 'syncing'"
        class="absolute inline-flex h-full w-full rounded-full opacity-60 animate-ping"
        :class="SYNC_DOT_CLASS[tone]"
      />
      <span class="relative inline-flex h-2 w-2 rounded-full" :class="SYNC_DOT_CLASS[tone]" />
    </span>
    {{ t('settings.sync.short') }}
  </button>
</template>
