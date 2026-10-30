<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useSyncStore } from '@/stores/sync'
import { ageParts, syncStateKey, syncTone } from '@/lib/syncCore'
import { isRemoteMode } from '@/lib/ipc/remote'

/**
 * Sidebar-footer dot for two-way sync: colour = state, tooltip = state + last pull + pending.
 * Hidden while this device is not in a sync group and in remote-desktop windows (it would show
 * the other machine's state). Click opens Settings → Sync.
 */

const { t } = useI18n()
const router = useRouter()
const store = useSyncStore()
const enabled = !isRemoteMode()

const status = computed(() => store.status)
const tone = computed(() => syncTone(status.value))

const DOT: Record<string, string> = {
  off: 'bg-muted-foreground/40',
  ok: 'bg-emerald-500',
  busy: 'bg-sky-500',
  warn: 'bg-amber-500',
  error: 'bg-destructive',
}

const tooltip = computed(() => {
  const s = status.value
  const { unit, n } = ageParts(s.last_pull)
  const parts = [
    `${t('settings.sync.title')}: ${t(`settings.sync.state.${syncStateKey(s)}`)}`,
    `${t('settings.sync.status.lastPull')}: ${t(`settings.sync.ago.${unit}`, { n })}`,
  ]
  if (s.pending) parts.push(`${t('settings.sync.status.pending')}: ${s.pending}`)
  if (s.last_error && s.state !== 'online') parts.push(s.last_error)
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
    v-if="enabled && status.state !== 'disabled'"
    type="button"
    class="flex items-center gap-1.5 rounded px-1 text-[10px] text-muted-foreground/70 hover:text-foreground"
    :title="tooltip"
    :aria-label="tooltip"
    @click="openSettings"
  >
    <span class="relative flex h-2 w-2">
      <span
        v-if="tone === 'busy'"
        class="absolute inline-flex h-full w-full rounded-full opacity-60 animate-ping"
        :class="DOT[tone]"
      />
      <span class="relative inline-flex h-2 w-2 rounded-full" :class="DOT[tone]" />
    </span>
    {{ t('settings.sync.short') }}
  </button>
</template>
