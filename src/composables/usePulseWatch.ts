// Watch / unwatch a source in Project Pulse from anywhere (Pulse screen, Slack
// threads window, Gantt rows, RunView): state comes from the window's pulse
// store (kept in sync by `pulse:changed`), unwatching asks first (§3.2), and
// failures become toasts in the user's language (§7).
import { useI18n } from 'vue-i18n'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { usePulseStore } from '@/stores/pulse'
import { pulseErrorCode, sourceKey, KNOWN_ERRORS, type PulseSourceType, type PulseWatchItem } from '@/lib/pulse'

export interface PulseSource {
  type: PulseSourceType
  /** Normalised `source_ref` (BR-02), null when not resolvable. */
  ref: string | null
  /** Link handed to `pulse_watch`. */
  url: string | null
}

export function usePulseWatch() {
  const { t, te } = useI18n()
  const store = usePulseStore()
  const { confirm } = useConfirm()
  const { toast } = useToast()

  void store.start()

  /** Message for a `"<code>: <detail>"` command error. */
  function errorMessage(e: unknown): string {
    const { code, detail } = pulseErrorCode(e)
    if (code === 'watched_in_other_project') {
      const project = /in project (.+)$/.exec(detail)?.[1] ?? ''
      return t('pulse.errors.watched_in_other_project', { project })
    }
    if (code && (KNOWN_ERRORS as readonly string[]).includes(code)) return t(`pulse.errors.${code}`)
    // Slack token / workspace problems keep the Slack window's messages (§7).
    if (code && te(`slackThread.errors.${code}`)) return t(`slackThread.errors.${code}`, { detail })
    return t('pulse.errors.unknown', { detail: code ? `${code}: ${detail}` : detail })
  }

  function watchOf(src: PulseSource): PulseWatchItem | undefined {
    return src.ref ? store.watched.get(sourceKey(src.type, src.ref)) : undefined
  }

  /** Watch in `projectId`; returns false on failure (already toasted). */
  async function watch(src: PulseSource, projectId: string, via: 'button' | 'paste' = 'button'): Promise<boolean> {
    if (!src.url) return false
    try {
      await store.watch(src.url, projectId, via)
      toast.success(t('pulse.watch.watched'))
      return true
    } catch (e) {
      toast.error(errorMessage(e))
      return false
    }
  }

  /** Unwatch after a light confirm; returns whether it was unwatched. */
  async function unwatch(watchId: string): Promise<boolean> {
    const ok = await confirm({
      title: t('pulse.watch.confirmTitle'),
      message: t('pulse.watch.confirm'),
      confirmLabel: t('pulse.watch.confirmButton'),
      cancelLabel: t('common.cancel'),
    })
    if (!ok) return false
    try {
      await store.unwatch(watchId)
      toast.success(t('pulse.watch.unwatched'))
      return true
    } catch (e) {
      toast.error(errorMessage(e))
      return false
    }
  }

  /** Toggle: unwatch when watched, else watch in `projectId`. */
  async function toggle(src: PulseSource, projectId: string | null): Promise<void> {
    const w = watchOf(src)
    if (w) {
      await unwatch(w.id)
      return
    }
    if (!projectId) {
      toast.error(t('pulse.watch.needProject'))
      return
    }
    await watch(src, projectId)
  }

  return { store, watchOf, watch, unwatch, toggle, errorMessage }
}
