// Pure helpers behind the Servers screen: sidebar filtering (search, tag,
// status) and grouping by first tag, the tag-chip editor's parsing, the SSH
// command for the ⋯ menu, and the "Test all" summary.
import { groupRows, haystack, matchesSearch, splitTags, type RowGroup } from './listGroups.ts'

export type ServerStatus = 'online' | 'offline' | 'unknown'

/** The subset of the store's VpsServer these helpers read. */
export interface ServerLike {
  id: string
  label: string
  host: string
  port: number
  username: string
  tags: string | null
  status: 'online' | 'offline' | 'unknown' | null
}

export interface ServerFilters {
  search: string
  /** '' = every tag. */
  tag: string
}

/** A never-tested server (null status) counts as unknown. */
export function serverStatus(server: Pick<ServerLike, 'status'>): ServerStatus {
  return server.status === 'online' || server.status === 'offline' ? server.status : 'unknown'
}

export function matchesServer(server: ServerLike, filters: ServerFilters): boolean {
  const tags = splitTags(server.tags)
  if (filters.tag && !tags.includes(filters.tag)) return false
  const hay = haystack(server.label, server.host, server.username, ...tags)
  return matchesSearch(hay, filters.search)
}

export function filterServers<T extends ServerLike>(servers: readonly T[], filters: ServerFilters): T[] {
  return servers.filter((s) => matchesServer(s, filters))
}

/** Every tag in use, A–Z. */
export function allServerTags(servers: readonly ServerLike[]): string[] {
  const set = new Set<string>()
  for (const s of servers) for (const tag of splitTags(s.tags)) set.add(tag)
  return [...set].sort((a, b) => a.localeCompare(b))
}

/** Sidebar folders: by first tag, untagged last. */
export function groupServers<T extends ServerLike>(servers: readonly T[], untaggedLabel: string): RowGroup<T>[] {
  return groupRows(servers, (s) => splitTags(s.tags)[0] ?? null, (key) => key, untaggedLabel)
}

/** Adds the comma-separated tags typed in `input` to `tags` (trimmed, no duplicates). */
export function addTags(tags: readonly string[], input: string): string[] {
  const out = [...tags]
  for (const tag of splitTags(input)) if (!out.includes(tag)) out.push(tag)
  return out
}

/** Chips → the stored comma-separated string (none → null). */
export function joinTags(tags: readonly string[]): string | null {
  const clean = splitTags(tags.join(','))
  return clean.length > 0 ? clean.join(', ') : null
}

export function sshCommand(server: Pick<ServerLike, 'host' | 'port' | 'username'>): string {
  return `ssh -p ${server.port} ${server.username}@${server.host}`
}

/** Counts the results of a "Test all" run (a rejected test counts as offline). */
export function summarizeTests(results: ReadonlyArray<PromiseSettledResult<{ ok: boolean }>>): {
  online: number
  offline: number
} {
  let online = 0
  for (const r of results) if (r.status === 'fulfilled' && r.value.ok) online++
  return { online, offline: results.length - online }
}
