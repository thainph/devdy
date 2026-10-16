// The Notes / Todos window's sidebar row, built from a note or a todo.
import { firstLine } from '@/lib/itemText'
import type { Note } from '@/stores/notes'
import type { Todo } from '@/stores/todos'

export interface ItemRow {
  id: string
  /** '' falls back to "Untitled" when rendered. */
  title: string
  /** Notes: the body's first prose line after the title ('' for todos). */
  preview: string
  done: boolean
  projectId: string | null
  linkedRunCount: number
  /** ISO time of the age shown (note: last edit, todo: creation). */
  at: string
  /** Lower-cased text the search box matches. */
  hay: string
}

export function noteRow(n: Note): ItemRow {
  const ownTitle = n.title.trim()
  const lines = n.content.split('\n').map(firstLine).filter(Boolean)
  return {
    id: n.id,
    title: ownTitle || lines[0] || '',
    // With the title taken from the body, its first line is not repeated.
    preview: (ownTitle ? lines : lines.slice(1)).join(' '),
    done: false,
    projectId: n.project_id,
    linkedRunCount: n.linked_run_count,
    at: n.updated_at,
    hay: `${n.title}\n${n.content}`.toLowerCase(),
  }
}

export function todoRow(x: Todo): ItemRow {
  return {
    id: x.id,
    title: firstLine(x.text),
    preview: '',
    done: x.done,
    projectId: x.project_id,
    linkedRunCount: x.linked_run_count,
    at: x.created_at,
    hay: x.text.toLowerCase(),
  }
}
