// Shared look of the list-detail sidebars' rows (ListDetailSidebar and the
// Notes / Todos ItemSidebar): hairline dividers between rows, a primary bar on
// the selected row, and a two-level text scale (title / secondary line).

/** Wrapper of one group: a little air between folders. */
export const groupClass = 'mb-3 last:mb-0'

/** Folder header row (below it, a hairline separates it from its rows). */
export const groupHeaderClass = 'group/folder flex items-center rounded border-b border-border/40 pb-0.5 hover:bg-accent/50'

/** The rows of one group, separated by hairlines. */
export const rowListClass = 'mt-1 divide-y divide-border/40'

const rowBase = 'relative flex w-full items-start rounded-md py-2.5 transition-colors'

export function rowClass(selected: boolean): string {
  return selected
    ? `${rowBase} bg-primary/10 text-foreground before:absolute before:inset-y-2 before:left-0 before:w-0.5 before:rounded-full before:bg-primary`
    : `${rowBase} text-foreground/85 hover:bg-accent/50`
}

/** A row's main line. */
export const rowTitleClass = 'text-sm font-medium leading-snug'

/** A row's secondary line (description, preview, user@host…). */
export const rowSubClass = 'mt-1 truncate text-[11px] text-muted-foreground/70'
