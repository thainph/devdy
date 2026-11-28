// Tailwind classes shared by the Delivery Radar's issue view (mode A) and
// milestone view (mode B): status tones and the issue row grid.
import type { MilestoneHealth, RadarStatus } from '@/lib/deliveryRadar'

export const TONE_TEXT: Record<RadarStatus, string> = {
  overdue: 'text-destructive',
  'due-soon': 'text-warning',
  'not-started': 'text-info',
  stale: 'text-foreground',
  'no-deadline': 'text-muted-foreground',
  'on-track': 'text-success',
}
export const TONE_CHIP: Record<RadarStatus, string> = {
  overdue: 'bg-destructive/12 text-destructive',
  'due-soon': 'bg-warning/12 text-warning',
  'not-started': 'bg-info/12 text-info',
  stale: 'bg-muted text-foreground',
  'no-deadline': 'bg-muted/70 text-muted-foreground',
  'on-track': 'bg-success/12 text-success',
}
export const TONE_BG: Record<RadarStatus, string> = {
  overdue: 'bg-destructive',
  'due-soon': 'bg-warning',
  'not-started': 'bg-info',
  stale: 'bg-foreground/70',
  'no-deadline': 'bg-muted-foreground/60',
  'on-track': 'bg-success',
}

// Milestone health: at risk is a softer (dashed) warning than "behind schedule".
export const HEALTH_TEXT: Record<MilestoneHealth, string> = {
  overdue: 'text-destructive',
  lag: 'text-warning',
  'at-risk': 'text-warning',
  'due-soon': 'text-info',
  'on-track': 'text-success',
  'no-due': 'text-muted-foreground',
  done: 'text-muted-foreground',
}
export const HEALTH_CHIP: Record<MilestoneHealth, string> = {
  overdue: 'bg-destructive/12 text-destructive',
  lag: 'bg-warning/12 text-warning',
  'at-risk': 'bg-warning/8 text-warning border border-dashed border-warning/45',
  'due-soon': 'bg-info/12 text-info',
  'on-track': 'bg-success/12 text-success',
  'no-due': 'bg-muted/70 text-muted-foreground',
  done: 'bg-muted text-muted-foreground',
}
export const HEALTH_BG: Record<MilestoneHealth, string> = {
  overdue: 'bg-destructive',
  lag: 'bg-warning',
  'at-risk': 'bg-warning/55',
  'due-soon': 'bg-info',
  'on-track': 'bg-success',
  'no-due': 'bg-muted-foreground/60',
  done: 'bg-success/60',
}

// Narrow panes drop the board-status column (it moves to the row's second line).
export const IGRID = 'grid grid-cols-[minmax(0,1fr)_minmax(96px,124px)_64px] @[36rem]/screen:grid-cols-[minmax(0,1fr)_minmax(104px,140px)_minmax(96px,124px)_minmax(64px,88px)] items-center gap-x-4'
