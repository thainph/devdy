// The Slack threads sidebar's view: what the search box's filter menu sets
// (activity, project, sort) — its defaults, which threads pass it, the
// per-activity counts the menu shows, the row order and the "Filtering: …"
// summary line. Pure (relative type imports only), so the sidebar and the
// window agree and it is testable with `node --test`.
import type { SlackThreadSummary } from '../stores/slackThreads.ts'
import { matchesActivity, type ActivityFilter } from './slackThreadUtils.ts'

/**
 * 'channel' = the workspace → #channel tree (threads by position); 'recent' =
 * one flat list, latest activity first; 'title' = flat, A–Z.
 */
export type SlackSortMode = 'channel' | 'recent' | 'title'

export interface SlackListView {
  activity: ActivityFilter
  /** '' = every project, SLACK_NO_PROJECT = threads without one, else a project id. */
  project: string
  sort: SlackSortMode
}

export const SLACK_NO_PROJECT = '__none__'

export const SLACK_ACTIVITIES: ActivityFilter[] = ['', 'unread', 'recent']
export const SLACK_SORTS: SlackSortMode[] = ['recent', 'channel', 'title']

export function defaultSlackListView(): SlackListView {
  return { activity: '', project: '', sort: 'recent' }
}

/** Read a remembered sort (the only part of the view kept between sessions). */
export function parseSlackSort(raw: unknown): SlackSortMode {
  return SLACK_SORTS.includes(raw as SlackSortMode) ? (raw as SlackSortMode) : defaultSlackListView().sort
}

/** Activity or project narrows the list (what the summary line and "Clear filters" are about). */
export function isSlackFiltering(view: SlackListView): boolean {
  return !!view.activity || !!view.project
}

/** Anything differs from the defaults: the filter button shows its accent dot. */
export function slackViewDiffersFromDefault(view: SlackListView): boolean {
  return isSlackFiltering(view) || view.sort !== defaultSlackListView().sort
}

export function clearSlackFilters(view: SlackListView): SlackListView {
  return { ...view, activity: '', project: '' }
}

type ViewFields = Pick<SlackThreadSummary, 'id' | 'projectId' | 'newCount' | 'lastReplyAt' | 'updatedAt'>

export function inSlackProject(th: Pick<SlackThreadSummary, 'projectId'>, project: string): boolean {
  if (!project) return true
  if (project === SLACK_NO_PROJECT) return !th.projectId
  return th.projectId === project
}

/**
 * The thread passes the view. `keepId` (the open thread) ignores the activity
 * filter: opening it marks it read, which would drop it from "Unread".
 */
export function matchesSlackView(
  th: ViewFields,
  view: SlackListView,
  now = Date.now(),
  keepId: string | null = null,
): boolean {
  return inSlackProject(th, view.project) && (th.id === keepId || matchesActivity(th, view.activity, now))
}

/** Counts behind each activity of the filter menu: the project in scope, before the activity filter. */
export function slackActivityCounts(
  threads: ViewFields[],
  view: SlackListView,
  now = Date.now(),
): Record<ActivityFilter, number> {
  const counts = { '': 0, unread: 0, recent: 0 } as Record<ActivityFilter, number>
  for (const th of threads) {
    if (!inSlackProject(th, view.project)) continue
    for (const a of SLACK_ACTIVITIES) if (matchesActivity(th, a, now)) counts[a]++
  }
  return counts
}

/** Flat-list order: 'recent' latest activity first, 'title' A–Z (untitled last). 'channel' keeps the order. */
export function sortSlackThreads<T extends Pick<SlackThreadSummary, 'title' | 'lastReplyAt' | 'updatedAt'>>(
  threads: T[],
  sort: SlackSortMode,
): T[] {
  if (sort === 'channel') return threads.slice()
  return threads.slice().sort((a, b) => {
    if (sort === 'recent') {
      const x = a.lastReplyAt ?? a.updatedAt
      const y = b.lastReplyAt ?? b.updatedAt
      return x < y ? 1 : x > y ? -1 : 0
    }
    return (!a.title ? 1 : 0) - (!b.title ? 1 : 0) || a.title.localeCompare(b.title)
  })
}

/** "Unread · icontruction": the parts of the "Filtering:" line, empty when not filtering. */
export function slackFilterSummary(
  view: SlackListView,
  labels: { activity: (a: ActivityFilter) => string; project: (id: string) => string },
): string[] {
  const parts: string[] = []
  if (view.activity) parts.push(labels.activity(view.activity))
  if (view.project) parts.push(labels.project(view.project))
  return parts
}
