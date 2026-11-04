// "Last synced: yesterday 14:32 from mac-mini": the last successful device sync pull on
// this machine, as one localized line (Settings → Devices and the sidebar Remote popover).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { syncDay } from '@/lib/deviceSync'

export function useLastSync() {
  const { t } = useI18n()
  const store = useRemoteControlStore()

  /** `null` when this machine never pulled. */
  const label = computed(() => {
    const last = store.lastSync
    if (!last) return null
    const d = new Date(last.at)
    const day = syncDay(last.at)
    if (!day) return null
    const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    const when = day === 'earlier'
      ? t('deviceSync.last.earlier', { date: d.toLocaleDateString(), time })
      : t(`deviceSync.last.${day}`, { time })
    const name = last.sourceAlias || `Devdy · ${last.sourceDeviceId.slice(0, 8)}`
    return t('deviceSync.last.line', { when, name })
  })

  function refresh() {
    store.refreshLastSync().catch(() => {})
  }

  return { label, refresh }
}
