// Shared look for DatePicker + DateRangePicker so both popovers stay in sync.
// radix-vue Calendar parts expose state via data-* attributes (data-selected,
// data-today, data-outside-view, data-disabled, …) — style through those.
import { controlSize } from './controlStyles'

/** Trigger button: same box as AppSelect's default variant. */
export const pickerTrigger = (size: keyof typeof controlSize) => [
  'inline-flex items-center gap-2 rounded-md border border-border bg-background text-foreground transition-colors cursor-pointer select-none',
  'hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
  'disabled:opacity-50 disabled:cursor-not-allowed data-[state=open]:border-ring data-[state=open]:ring-1 data-[state=open]:ring-ring',
  controlSize[size],
]

export const popoverContent =
  'z-50 rounded-md border border-border bg-popover p-3 text-popover-foreground shadow-lg shadow-black/20 outline-none'

export const calendarHeader = 'flex items-center justify-between gap-2 pb-2'
export const calendarHeading = 'text-xs font-semibold capitalize text-foreground'
export const calendarNav =
  'inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground transition-colors cursor-pointer ' +
  'hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring ' +
  'disabled:opacity-30 disabled:pointer-events-none'
export const calendarGrid = 'w-full border-collapse select-none'
export const calendarHeadCell = 'h-7 w-8 text-[10px] font-medium uppercase text-muted-foreground'
export const calendarCell = 'p-0 text-center'

const cellBase =
  'relative inline-flex h-8 w-8 items-center justify-center text-xs text-foreground transition-colors cursor-pointer ' +
  'hover:bg-accent focus:outline-none focus-visible:ring-1 focus-visible:ring-ring ' +
  'data-[outside-view]:text-muted-foreground/40 ' +
  'data-[disabled]:text-muted-foreground/30 data-[disabled]:pointer-events-none ' +
  'data-[unavailable]:line-through data-[unavailable]:text-muted-foreground/30 ' +
  // Today: small dot under the number (flips to foreground colour when selected).
  'data-[today]:font-semibold data-[today]:after:absolute data-[today]:after:bottom-1 data-[today]:after:h-1 ' +
  'data-[today]:after:w-1 data-[today]:after:rounded-full data-[today]:after:bg-primary ' +
  'data-[selected]:data-[today]:after:bg-primary-foreground '

/** Single-date cell. */
export const dayCell =
  cellBase +
  'rounded-md data-[selected]:bg-primary data-[selected]:text-primary-foreground data-[selected]:hover:bg-primary'

/**
 * Range cell: tinted band between the endpoints, solid endpoints. Spill-over
 * days are hidden — with two months side by side they'd duplicate the band.
 */
export const rangeDayCell =
  cellBase +
  'rounded-md data-[outside-view]:invisible ' +
  'data-[selected]:rounded-none data-[selected]:bg-primary/15 data-[highlighted]:rounded-none data-[highlighted]:bg-primary/10 ' +
  'data-[selection-start]:rounded-l-md data-[selection-end]:rounded-r-md ' +
  'data-[selection-start]:bg-primary data-[selection-start]:text-primary-foreground ' +
  'data-[selection-end]:bg-primary data-[selection-end]:text-primary-foreground ' +
  'data-[selection-start]:data-[today]:after:bg-primary-foreground data-[selection-end]:data-[today]:after:bg-primary-foreground'
