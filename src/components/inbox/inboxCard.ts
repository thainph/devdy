// Shared sizing of the inbox lists (PR Inbox, GitHub Mentions — InboxCard).

/** The primary action's label shows from this list width up; below it the
 * button is icon-only (title + aria-label kept). */
export function actionLabelClass(compact: boolean): string {
  return compact ? 'hidden @[44rem]/list:inline' : 'hidden @[36rem]/list:inline'
}
