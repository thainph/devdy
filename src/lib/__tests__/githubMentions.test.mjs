// Unit tests for lib/githubMentions.ts (filters, search, grouping, split).
// Run: node --test src/lib/__tests__/githubMentions.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  activeFilters,
  applyFilters,
  checkStatusOf,
  filterOptions,
  groupMentions,
  isStale,
  quoteExcerpt,
  sanitizeFilters,
  splitAnswered,
} from '../githubMentions.ts'

function item(over = {}) {
  return {
    key: 'a1:1', thread_id: '1', account_id: 'a1', account_label: 'work', account_login: 'me',
    reason: 'mention', unread: true, kind: 'Issue', owner: 'org', repo: 'api', number: 7,
    title: 'Fix sync', html_url: 'https://github.com/org/api/issues/7', updated_at: '2026-11-10T00:00:00Z',
    trigger: { id: 9, author_login: 'lam', author_avatar_url: '', body: '@me check this', html_url: '', created_at: '2026-11-10T00:00:00Z', review_comment: false, path: null },
    newer_comments: 0, answered: false, answered_at: null,
    project_id: 'p1', repo_id: 'r1', project_name: 'API', existing_run_id: null, existing_run_status: null,
    ...over,
  }
}

test('filters by reason, account and repo; search hits title, author and body', () => {
  const items = [
    item(),
    item({ key: 'a2:2', account_id: 'a2', reason: 'team_mention', repo: 'web', title: 'Layout' }),
  ]
  assert.equal(applyFilters(items, { reasons: ['team_mention'], accounts: [], repos: [] }, '').length, 1)
  assert.equal(applyFilters(items, { reasons: [], accounts: ['a1'], repos: [] }, '').length, 1)
  assert.equal(applyFilters(items, { reasons: [], accounts: [], repos: ['org/web'] }, '').length, 1)
  assert.equal(applyFilters(items, sanitizeFilters(null), 'LAM').length, 2)
  assert.equal(applyFilters(items, sanitizeFilters(null), 'layout').length, 1)
  assert.equal(applyFilters(items, sanitizeFilters(null), 'check this').length, 2)
})

test('options count items and stale selections are dropped', () => {
  const items = [item(), item({ key: 'b', repo: 'web' }), item({ key: 'c', repo: 'web' })]
  const opts = filterOptions(items)
  assert.deepEqual(opts.repos.map(o => [o.value, o.count]), [['org/web', 2], ['org/api', 1]])
  assert.deepEqual(activeFilters({ reasons: ['gone'], accounts: [], repos: ['org/api'] }, items).repos, ['org/api'])
  assert.deepEqual(activeFilters({ reasons: ['gone'], accounts: [], repos: [] }, items).reasons, [])
})

test('groups by repo / person keep first-seen order', () => {
  const items = [item({ repo: 'web' }), item({ key: 'b' }), item({ key: 'c', repo: 'web', trigger: { ...item().trigger, author_login: 'an' } })]
  assert.deepEqual(groupMentions(items, 'repo').map(g => [g.label, g.items.length]), [['org/web', 2], ['org/api', 1]])
  assert.deepEqual(groupMentions(items, 'person').map(g => g.label), ['lam', 'an'])
  assert.equal(groupMentions([], 'none').length, 0)
})

test('split puts answered apart, newest first', () => {
  const items = [
    item({ key: 'old', trigger: { ...item().trigger, created_at: '2026-11-01T00:00:00Z' } }),
    item({ key: 'new', trigger: { ...item().trigger, created_at: '2026-11-12T00:00:00Z' } }),
    item({ key: 'done', answered: true, answered_at: '2026-11-11T00:00:00Z' }),
  ]
  const { pending, answered } = splitAnswered(items)
  assert.deepEqual(pending.map(i => i.key), ['new', 'old'])
  assert.deepEqual(answered.map(i => i.key), ['done'])
})

test('stale only when waiting more than 3 days', () => {
  const now = Date.parse('2026-11-14T01:00:00Z')
  assert.equal(isStale(item(), now), true)
  assert.equal(isStale(item({ answered: true }), now), false)
  assert.equal(isStale(item(), Date.parse('2026-11-11T00:00:00Z')), false)
})

test('check status: unmapped, unavailable, startable, running, done', () => {
  assert.equal(checkStatusOf(item(), null), 'none')
  assert.equal(checkStatusOf(item({ project_id: null, repo_id: null }), null), 'unmapped')
  assert.equal(checkStatusOf(item({ kind: 'Discussion', number: null }), null), 'unavailable')
  assert.equal(checkStatusOf(item({ existing_run_id: 'run' }), 'running'), 'running')
  assert.equal(checkStatusOf(item({ existing_run_id: 'run' }), 'fetched'), 'running')
  assert.equal(checkStatusOf(item({ existing_run_id: 'run' }), 'completed'), 'done')
})

test('excerpt becomes a Markdown quote', () => {
  assert.equal(quoteExcerpt('a\nb'), '> a\n> b')
  assert.equal(quoteExcerpt('  '), '')
})
