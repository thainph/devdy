// Icon + label of each surface, for its tab and menus (lib/surfaces is pure
// logic and stays free of UI imports).
import type { Component } from 'vue'
import { AtSign, CalendarDays, FileCode, GitPullRequest, ListTodo, Radar, Slack, StickyNote } from 'lucide-vue-next'
import type { SurfaceKind } from '@/lib/surfaces'

export const SURFACE_META: Record<SurfaceKind, { icon: Component; labelKey: string }> = {
  prInbox: { icon: GitPullRequest, labelKey: 'nav.prInbox' },
  mentions: { icon: AtSign, labelKey: 'nav.githubMentions' },
  calendar: { icon: CalendarDays, labelKey: 'nav.calendar' },
  radar: { icon: Radar, labelKey: 'nav.radar' },
  slack: { icon: Slack, labelKey: 'slackThread.nav' },
  todos: { icon: ListTodo, labelKey: 'item.navTodos' },
  notes: { icon: StickyNote, labelKey: 'item.navNotes' },
  file: { icon: FileCode, labelKey: 'surfaces.file' },
}
