// Pure logic behind the Remote Controller's Todo and Note screens: splitting a
// todo's text into title + note, filtering and grouping the list by deadline,
// and the due-date shortcut chips. Tauri- and i18n-free so node:test can run it.
//
// Dates: a todo's `due_date` is a plain calendar date (`YYYY-MM-DD`, no time
// or zone) — the day the user picked, stored as-is by the Host (the desktop's
// "local date"). It is compared, as a string-derived day number, against the
// PHONE's local calendar date (`localYmd`); no Date/zone conversion ever
// touches it, so it never shifts a day. A phone in another time zone than the
// desktop sees Today / Overdue by its own clock, as a paper diary would.

import { daysUntil } from './todoDue.ts'

/** The fields of a todo these helpers read (a subset of the wire `TodoInfo`). */
export interface TodoLike {
  id: string
  text: string
  done: boolean
  project_id?: string | null
  due_date?: string | null
  created_at: string
  completed_at?: string | null
}

/** A todo's text as the phone edits it: the first line is the title, the rest a note. */
export function splitTodoText(text: string): { title: string; note: string } {
  const i = text.indexOf('\n')
  if (i < 0) return { title: text.trim(), note: '' }
  return { title: text.slice(0, i).trim(), note: text.slice(i + 1).trim() }
}

/** Inverse of {@link splitTodoText}; blank when there is no title. */
export function joinTodoText(title: string, note: string): string {
  const t = title.trim().replace(/\s*\n\s*/g, ' ')
  if (!t) return ''
  const n = note.trim()
  return n ? `${t}\n${n}` : t
}

export type TodoTab = 'active' | 'done'

/** Todos in `projectId` (null = every project). */
export function filterByProject<T extends TodoLike>(todos: T[], projectId: string | null): T[] {
  return projectId ? todos.filter((t) => t.project_id === projectId) : todos
}

/** Open / done counts for the segmented control. */
export function countTodos(todos: TodoLike[]): Record<TodoTab, number> {
  let done = 0
  for (const t of todos) if (t.done) done += 1
  return { active: todos.length - done, done }
}

/** Sections of the open list, in display order. `upcoming` holds dated todos
 * past today so a future deadline never drops off the list. */
export type TodoGroupKey = 'overdue' | 'today' | 'upcoming' | 'none'
export const TODO_GROUP_ORDER: TodoGroupKey[] = ['overdue', 'today', 'upcoming', 'none']

export interface TodoGroup<T> {
  key: TodoGroupKey
  todos: T[]
}

/** The group of an open todo against `today` (`YYYY-MM-DD`). */
export function todoGroup(todo: TodoLike, today: string): TodoGroupKey {
  const days = daysUntil(todo.due_date, today)
  if (days == null) return 'none'
  if (days < 0) return 'overdue'
  if (days === 0) return 'today'
  return 'upcoming'
}

/**
 * Open todos grouped by deadline, non-empty groups only. Dated groups run
 * earliest deadline first; ties (and the undated group) keep the Host's order,
 * which is the desktop's manual order.
 */
export function groupOpenTodos<T extends TodoLike>(todos: T[], today: string): TodoGroup<T>[] {
  const buckets = new Map<TodoGroupKey, T[]>(TODO_GROUP_ORDER.map((k) => [k, []]))
  for (const t of todos) {
    if (!t.done) buckets.get(todoGroup(t, today))!.push(t)
  }
  const byDue = (a: T, b: T) => (a.due_date ?? '').localeCompare(b.due_date ?? '')
  return TODO_GROUP_ORDER.map((key) => {
    const list = buckets.get(key)!
    return { key, todos: key === 'none' ? list : [...list].sort(byDue) }
  }).filter((g) => g.todos.length > 0)
}

/** Done todos, most recently completed first. */
export function doneTodos<T extends TodoLike>(todos: T[]): T[] {
  return todos
    .filter((t) => t.done)
    .sort((a, b) => (b.completed_at ?? b.created_at).localeCompare(a.completed_at ?? a.created_at))
}

/** The due-date shortcut chips in the edit sheet. */
export type DueChip = 'today' | 'tomorrow' | 'next_week' | 'clear'
export const DUE_CHIPS: DueChip[] = ['today', 'tomorrow', 'next_week', 'clear']

/** `ymd` moved by `days` calendar days. */
export function addDays(ymd: string, days: number): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd)
  if (!m) return ymd
  const d = new Date(Date.UTC(+m[1], +m[2] - 1, +m[3] + days))
  return d.toISOString().slice(0, 10)
}

/** The date a chip sets, `''` for "clear". "Next week" is a week from today. */
export function dueFromChip(chip: DueChip, today: string): string {
  switch (chip) {
    case 'today':
      return today
    case 'tomorrow':
      return addDays(today, 1)
    case 'next_week':
      return addDays(today, 7)
    case 'clear':
      return ''
  }
}

/** The chip that matches `due` (to highlight it), or null. */
export function activeDueChip(due: string, today: string): DueChip | null {
  return DUE_CHIPS.find((c) => dueFromChip(c, today) === due) ?? null
}

/** A note's display title: its own title, else the first content line. */
export function noteDisplayTitle(note: { title: string; content: string }): string {
  const title = note.title.trim()
  if (title) return title
  const line = note.content.split('\n').find((l) => l.trim()) ?? ''
  return line.replace(/^\s*(#{1,6}|[-*+]|>)\s+/, '').trim()
}

/** A one-paragraph plain preview of a note body (Markdown markers dropped). */
export function notePreview(content: string, max = 160): string {
  const flat = content
    .split('\n')
    .map((l) => l.replace(/^\s*(#{1,6}|[-*+]\s+\[[ xX]\]|[-*+]|>|\d+\.)\s+/, '').trim())
    .filter(Boolean)
    .join(' ')
    .replace(/[*_`]+/g, '')
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat
}

/** The list preview of a note: its body, minus the first line when that line
 * already serves as the title. */
export function noteListPreview(note: { title: string; content: string }, max = 160): string {
  if (note.title.trim()) return notePreview(note.content, max)
  const lines = note.content.split('\n')
  const first = lines.findIndex((l) => l.trim())
  return notePreview(first < 0 ? '' : lines.slice(first + 1).join('\n'), max)
}
