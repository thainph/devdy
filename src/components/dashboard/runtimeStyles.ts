// Runtime Dashboard — state colours (theme tokens only, so every color theme
// and light / dark mode read correctly) and the localized labels shared by the
// cards, rows, groups and the detail panel.
import { useI18n } from 'vue-i18n'
import { formatAge } from '@/lib/backgroundProcesses'
import { formatElapsed, toolLabel, type RtSession, type SessionState } from '@/lib/runtimeDashboard'

/** Status pill. */
export const STATE_TAG: Record<SessionState, string> = {
  waiting: 'bg-warning/15 text-warning',
  quiet: 'bg-warning/15 text-warning',
  running: 'bg-success/15 text-success',
  idle: 'bg-muted text-muted-foreground',
  external: 'bg-info/15 text-info',
}

/** Small filled square / dot (session map, group dots). Quiet is hatched. */
export const STATE_FILL: Record<SessionState, string> = {
  waiting: 'bg-warning',
  quiet: 'bg-[repeating-linear-gradient(135deg,var(--color-warning)_0_2px,transparent_2px_4px)] ring-1 ring-inset ring-warning/70',
  running: 'bg-success',
  idle: 'bg-muted-foreground/40',
  external: 'bg-info/60',
}

/** Card / row frame by state; the selected one gets a thin primary border and tint. */
export function frameClass(s: Pick<RtSession, 'state'>, selected: boolean): string[] {
  const out: string[] = []
  if (s.state === 'waiting') out.push('border-warning/60 bg-warning/10')
  else if (s.state === 'quiet') out.push('border-dashed border-warning/70 bg-card')
  else out.push('border-border/70 bg-card')
  if (selected) out.push('border-primary/70! bg-primary/[0.06]!')
  else out.push('hover:border-primary/40')
  return out
}

/** The context ring's arc. */
export function ctxRingClass(pct: number | null): string {
  if (pct === null) return 'stroke-muted-foreground/30'
  return pct >= 80 ? 'stroke-warning' : 'stroke-primary/80'
}

export function ctxTextClass(pct: number | null): string {
  return pct !== null && pct >= 80 ? 'text-warning' : 'text-muted-foreground'
}

export function useRuntimeLabels() {
  const { t } = useI18n()

  function stateLabel(state: SessionState): string {
    return t(`dashboard.state.${state}`)
  }

  /** The "now" line: what the session is doing / asking. */
  function nowText(s: RtSession, now: number): string {
    const n = s.now
    switch (n.kind) {
      case 'permission':
        if (!n.tool) return t('dashboard.now.permissionUnknown')
        return n.detail
          ? t('dashboard.now.permission', { tool: toolLabel(n.tool), detail: n.detail })
          : t('dashboard.now.permissionBare', { tool: toolLabel(n.tool) })
      case 'question':
        return n.detail ? t('dashboard.now.question', { detail: n.detail }) : t('dashboard.now.questionBare')
      case 'wait':
        return n.detail ? `${t('dashboard.now.wait')} · ${n.detail}` : t('dashboard.now.wait')
      case 'tool':
        return n.detail ? `${toolLabel(n.tool ?? '')} ${n.detail}` : toolLabel(n.tool ?? '')
      case 'last':
        return n.detail
      default:
        if (s.state === 'external') {
          const age = n.since !== null ? formatAge(new Date(n.since).toISOString(), now) : ''
          return age ? t('dashboard.now.lastActivity', { age }) : ''
        }
        return t('dashboard.now.working')
    }
  }

  /** How long the current step has lasted (empty when unknown). */
  function nowFor(s: RtSession, now: number): string {
    return s.now.since !== null && s.state !== 'external' ? formatElapsed(now - s.now.since) : ''
  }

  function elapsed(s: RtSession, now: number): string {
    return s.startedAt !== null ? formatElapsed(now - s.startedAt) : ''
  }

  /** "project · worker" */
  function meta(s: RtSession): string {
    return [s.projectName, t(`dashboard.role.${s.role}`)].filter(Boolean).join(' · ')
  }

  function engineName(s: Pick<RtSession, 'engine'>): string {
    return t(`dashboard.engine.${s.engine}`)
  }

  return { stateLabel, nowText, nowFor, elapsed, meta, engineName }
}
