// Compact PageHeader actions. PageHeader is a `@container/page-header`: below
// 40rem (a split pane, a narrow window) its labelled buttons go icon-only and
// a search box collapses to an icon (PageHeaderSearch). Literal class strings
// so Tailwind sees them; see DESIGN_SYSTEM.md §4.

/** On a button's text label inside PageHeader actions: hidden while the header is narrow. */
export const pageHeaderLabel = 'hidden @[40rem]/page-header:inline'

/**
 * On a labelled `size="sm"` Button inside PageHeader actions: about square while
 * the label is hidden (still wide enough for a count pill after the icon).
 */
export const pageHeaderCompactButton = 'min-w-8 px-2 @[40rem]/page-header:px-3'

/**
 * Like `pageHeaderLabel`, for secondary buttons in a crowded header (Filters /
 * Display next to a search box): labelled only once the header is wide.
 */
export const pageHeaderWideLabel = 'hidden @[56rem]/page-header:inline'
