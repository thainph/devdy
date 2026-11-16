// Unit tests for lib/remotePrInbox.ts (Remote Controller PR tab).
// Run: node --test src/lib/__tests__/remotePrInbox.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  botCount,
  filterPrs,
  matchesPrQuery,
  prAction,
  prReviewState,
  reviewRunStatus,
} from '../remotePrInbox.ts'

function pr(over = {}) {
  return {
    key: 'acme/web#42',
    owner: 'Acme',
    repo: 'web',
    number: 42,
    title: 'Fix login redirect',
    author_login: 'octo',
    author_is_bot: false,
    labels: ['bug'],
    updated_at: '2026-11-10T00:00:00Z',
    mapped: true,
    project_id: 'p1',
    existing_run_id: null,
    existing_run_status: null,
    updated_since_review: false,
    ...over,
  }
}

test('the live run list wins over the inbox snapshot', () => {
  const linked = pr({ existing_run_id: 'r1', existing_run_status: 'running' })
  assert.equal(reviewRunStatus(linked, [{ id: 'r1', status: 'done' }]), 'done')
  assert.equal(reviewRunStatus(linked, [{ id: 'r2', status: 'done' }]), 'running')
  assert.equal(reviewRunStatus(pr(), [{ id: 'r1', status: 'done' }]), null)
})

test('review state follows the desktop', () => {
  assert.equal(prReviewState(pr({ mapped: false }), null), 'unmapped')
  assert.equal(prReviewState(pr(), null), 'not_reviewed')
  const linked = pr({ existing_run_id: 'r1' })
  assert.equal(prReviewState(linked, 'running'), 'running')
  assert.equal(prReviewState(linked, 'fetched'), 'running')
  assert.equal(prReviewState(linked, 'done'), 'reviewed')
  assert.equal(prReviewState({ ...linked, updated_since_review: true }, 'done'), 'updated')
  // A run still going is "running" even with new commits.
  assert.equal(prReviewState({ ...linked, updated_since_review: true }, 'running'), 'running')
})

test('the main button per state', () => {
  assert.equal(prAction('unmapped', false), 'none')
  assert.equal(prAction('not_reviewed', false), 'review')
  assert.equal(prAction('running', false), 'view')
  assert.equal(prAction('reviewed', false), 'review_again')
  assert.equal(prAction('updated', false), 'review_again')
  for (const s of ['unmapped', 'not_reviewed', 'running', 'reviewed', 'updated']) {
    assert.equal(prAction(s, true), 'starting')
  }
})

test('search matches number, repo, title, author and labels', () => {
  const p = pr()
  for (const q of ['', '42', '#42', 'web#42', 'acme/web', 'LOGIN', 'octo', 'bug', 'fix redirect']) {
    assert.ok(matchesPrQuery(p, q), q)
  }
  for (const q of ['43', 'logout', 'fix logout']) {
    assert.ok(!matchesPrQuery(p, q), q)
  }
})

test('filter: bots, project scope, newest first', () => {
  const items = [
    pr({ key: 'a#1', number: 1, updated_at: '2026-11-01T00:00:00Z' }),
    pr({ key: 'a#2', number: 2, updated_at: '2026-11-03T00:00:00Z', author_is_bot: true }),
    pr({ key: 'a#3', number: 3, updated_at: '2026-11-02T00:00:00Z', project_id: 'p2' }),
    pr({ key: 'a#4', number: 4, updated_at: '2026-11-04T00:00:00Z', mapped: false, project_id: null }),
  ]
  const keys = (f) => filterPrs(items, { query: '', hideBots: false, projectId: null, ...f }).map((p) => p.key)
  assert.deepEqual(keys({}), ['a#4', 'a#2', 'a#3', 'a#1'])
  assert.deepEqual(keys({ hideBots: true }), ['a#4', 'a#3', 'a#1'])
  assert.deepEqual(keys({ projectId: 'p1' }), ['a#2', 'a#1'])
  assert.deepEqual(keys({ projectId: 'p1', query: '#1' }), ['a#1'])
  assert.equal(botCount(items), 1)
  // The input is left as it was.
  assert.equal(items[0].key, 'a#1')
})
