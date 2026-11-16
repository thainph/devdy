// Unit tests for lib/prPriority.ts (Work Dashboard "PRs waiting for my review").
// Run: node --test src/lib/__tests__/prPriority.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  actionableCount, aiReviewBadge, ciStatus, prWaitLevel, rankPr, rankPrs, workdaysBetween,
} from '../prPriority.ts'

// Monday 2026-11-16 10:00 local time.
const NOW = new Date(2026, 10, 16, 10, 0).getTime()
const DAY = 86_400_000

let seq = 0
function pr(extra = {}) {
  seq += 1
  return {
    owner: 'acme', repo: 'web', number: seq, draft: false, author_login: 'bob', author_is_bot: false,
    updated_at: '2026-11-15T00:00:00Z', existing_run_id: null, existing_run_status: null,
    updated_since_review: false, requested_reviewers: [{ login_or_slug: 'carol' }], reviewers_known: true,
    additions: 500, deletions: 100, ci_state: null, review_requested_at: null,
    ...extra,
  }
}

test('workdaysBetween skips weekends', () => {
  // Friday 10:00 → Monday 10:00 = 1 working day.
  const fri = new Date(2026, 10, 13, 10, 0).getTime()
  assert.equal(workdaysBetween(fri, NOW), 1)
  // Saturday → Sunday: none.
  assert.equal(workdaysBetween(new Date(2026, 10, 14, 9).getTime(), new Date(2026, 10, 15, 20).getTime()), 0)
  assert.equal(workdaysBetween(NOW, NOW - DAY), 0)
})

test('wait levels: > 1 working day amber, > 3 red', () => {
  assert.equal(prWaitLevel(null), 'ok')
  assert.equal(prWaitLevel(1), 'ok')
  assert.equal(prWaitLevel(1.5), 'warn')
  assert.equal(prWaitLevel(3.1), 'late')
})

test('CI rollup states and AI review badges', () => {
  assert.deepEqual(['SUCCESS', 'FAILURE', 'ERROR', 'PENDING', 'EXPECTED', null].map(ciStatus),
    ['success', 'failure', 'failure', 'pending', 'pending', null])
  assert.equal(aiReviewBadge(pr()), 'none')
  assert.equal(aiReviewBadge(pr({ existing_run_id: 'r', existing_run_status: 'running' })), 'running')
  assert.equal(aiReviewBadge(pr({ existing_run_id: 'r', existing_run_status: 'done' })), 'done')
  assert.equal(aiReviewBadge(pr({ existing_run_id: 'r', existing_run_status: 'done', updated_since_review: true })), 'updated')
})

test('no review-request time → no wait (only "updated")', () => {
  const r = rankPr(pr(), NOW)
  assert.equal(r.waitDays, null)
  assert.equal(r.waitLevel, 'ok')
})

test('bonuses: green CI, small PR, last reviewer', () => {
  const r = rankPr(pr({ ci_state: 'SUCCESS', additions: 50, deletions: 20, requested_reviewers: [] }), NOW)
  assert.ok(r.small && r.soleReviewer)
  assert.equal(r.ci, 'success')
  assert.equal(r.score, 30)
})

test('an empty reviewer list from a failed lookup earns no "last reviewer" bonus', () => {
  const unknown = rankPr(pr({ requested_reviewers: [], reviewers_known: false }), NOW)
  assert.equal(unknown.soleReviewer, false)
  assert.equal(unknown.score, 0)
  // Caches from before the flag: unknown too.
  const legacy = pr({ requested_reviewers: [] })
  delete legacy.reviewers_known
  assert.equal(rankPr(legacy, NOW).soleReviewer, false)
  assert.equal(rankPr(pr({ requested_reviewers: [] }), NOW).soleReviewer, true)
})

test('order: re-review → long wait → bonuses; drafts last; bots apart; failing CI kept', () => {
  const ranked = rankPrs([
    pr({ number: 1, ci_state: 'SUCCESS', additions: 10, deletions: 0 }),
    pr({ number: 2, review_requested_at: new Date(NOW - 6 * DAY).toISOString() }),
    pr({ number: 3, existing_run_id: 'r', existing_run_status: 'done', updated_since_review: true }),
    pr({ number: 4, draft: true, review_requested_at: new Date(NOW - 9 * DAY).toISOString() }),
    pr({ number: 5, ci_state: 'FAILURE' }),
    pr({ number: 6, author_login: 'dependabot[bot]', author_is_bot: true }),
    pr({ number: 7, author_login: 'renovate' }),
  ], NOW, (p) => p.author_is_bot || p.author_login === 'renovate')
  assert.deepEqual(ranked.rows.map((r) => r.pr.number), [3, 2, 1, 5, 4])
  assert.equal(ranked.rows[1].waitLevel, 'late')
  assert.equal(ranked.rows[3].ci, 'failure')
  assert.deepEqual(ranked.bots.map((r) => r.pr.number).sort(), [6, 7])
  assert.equal(actionableCount(ranked), 4)
})
