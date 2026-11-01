// Single source of truth for form-control sizing + base styling.
//
// Shared by Input, Textarea, and AppSelect so a design tweak (padding, font,
// border, focus ring) lands in exactly ONE place and every screen stays in
// sync. Do NOT re-hardcode these class strings or override size via ad-hoc
// `class="px-… py-… text-…"` on a shared control — add/adjust a token here
// instead.

/** Horizontal/vertical padding + font per control size. */
export const controlSize = {
  /** Same font as Button `xs` (compact controls in a row of xs buttons). */
  xs: 'px-2.5 py-1 text-[10px]',
  sm: 'px-3 py-1.5 text-xs',
  md: 'px-3 py-2 text-sm',
} as const

export type ControlSize = keyof typeof controlSize

/**
 * Fixed height per control size — the same scale as Button (xs 28 / sm 32 /
 * md 36px), so a search field, a select and a button sitting in one row line
 * up exactly. Applied to single-line controls only (Input, AppSelect trigger,
 * date-picker trigger) — never to Textarea or dropdown options.
 */
export const controlHeight = {
  xs: 'h-7',
  sm: 'h-8',
  md: 'h-9',
} as const

/**
 * Base look for bordered text fields (Input, Textarea): background, border,
 * radius, focus ring, disabled + placeholder styling. Size is applied on top.
 */
export const fieldBase =
  'w-full bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring transition-colors disabled:opacity-50 disabled:cursor-not-allowed placeholder:text-muted-foreground'
