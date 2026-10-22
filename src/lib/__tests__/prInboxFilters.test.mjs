// Unit tests for lib/prInboxFilters.ts (filters, search, bot list, badges,
// bulk-review selection). Run: node --test src/lib/__tests__/prInboxFilters.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  UNMAPPED_PROJECT_VALUE,
  ambiguousRepoNames,
  applyFilters,
  buildFilterOptions,
  bulkReviewCandidates,
  canStartReview,
  effectiveAccountCount,
  makeAccountTone,
  effectiveFilters,
  emptyFilters,
  hasActiveFilters,
  isStale,
  makeBotPredicate,
  matchesSearch,
  reviewStatusOf,
  sanitizeBotLogins,
  sanitizeFilters,
  splitLabels,
} from '../prInboxFilters.ts'
import { AVATAR_TONES, avatarTone } from '../slackThreadUtils.ts'

let seq = 0
function pr(extra = {}) {
  seq += 1
  return {
    account_id: 'acc1',
    account_label: 'Work',
    owner: 'acme',
    repo: 'web',
    number: seq,
    title: `PR ${seq}`,
    html_url: `https://github.com/acme/web/pull/${seq}`,
    author_login: 'alice',
    author_avatar_url: 'https://avatars/alice',
    author_is_bot: false,
    draft: false,
    labels: [],
    updated_at: '2026-10-01T00:00:00+00:00',
    mapped: true,
    project_id: 'p1',
    repo_id: 'r1',
    project_name: 'Alpha',
    existing_run_id: null,
    existing_run_status: null,
    reviewer_accounts: [{ account_id: 'acc1', account_label: 'Work' }],
    requested_reviewers: [],
    ...extra,
  }
}

const unmapped = { mapped: false, project_id: null, repo_id: null, project_name: null }
const statusOf = item => reviewStatusOf(item, item.existing_run_status)

test('reviewStatusOf covers the four states', () => {
  assert.equal(reviewStatusOf(pr(unmapped), null), 'unmapped')
  assert.equal(reviewStatusOf(pr(), null), 'not_reviewed')
  assert.equal(reviewStatusOf(pr({ existing_run_id: 'r' }), 'running'), 'running')
  assert.equal(reviewStatusOf(pr({ existing_run_id: 'r' }), 'fetched'), 'running')
  assert.equal(reviewStatusOf(pr({ existing_run_id: 'r' }), 'done'), 'reviewed')
})

test('sanitizeFilters drops malformed shapes and unknown statuses', () => {
  assert.deepEqual(sanitizeFilters(null), emptyFilters())
  assert.deepEqual(sanitizeFilters([1, 2]), emptyFilters())
  assert.deepEqual(
    sanitizeFilters({
      projects: ['p1', 'p1', 3, ''],
      authors: [' Alice ', 'alice', 'BOB'],
      accounts: 'acc1',
      statuses: ['running', 'bogus', 'unmapped'],
    }),
    { projects: ['p1'], authors: ['alice', 'bob'], accounts: [], statuses: ['running', 'unmapped'] },
  )
})

test('sanitizeBotLogins trims, lowercases and dedups', () => {
  assert.deepEqual(sanitizeBotLogins([' CI-Bot ', 'ci-bot', '', 42, 'deployer']), ['ci-bot', 'deployer'])
  assert.deepEqual(sanitizeBotLogins({ a: 1 }), [])
})

test('makeBotPredicate = backend flag OR custom list (case-insensitive)', () => {
  const isBot = makeBotPredicate(['CI-Service'])
  assert.equal(isBot(pr({ author_is_bot: true, author_login: 'x' })), true)
  assert.equal(isBot(pr({ author_login: 'ci-service' })), true)
  assert.equal(isBot(pr({ author_login: ' CI-SERVICE ' })), true)
  assert.equal(isBot(pr({ author_login: 'alice' })), false)
})

test('buildFilterOptions counts per dimension; unmapped project last', () => {
  const items = [
    pr({ author_login: 'Bob', updated_at: '2026-10-02T00:00:00+00:00' }),
    pr({ ...unmapped }),
    pr({ ...unmapped }),
    pr({ project_id: 'p2', project_name: '  ', existing_run_id: 'r', existing_run_status: 'done',
      reviewer_accounts: [{ account_id: 'acc1', account_label: 'Work' }, { account_id: 'acc2', account_label: 'Personal' }] }),
  ]
  const o = buildFilterOptions(items, statusOf)
  assert.deepEqual(o.projects.map(x => [x.value, x.label, x.count]), [
    ['p1', 'Alpha', 1],
    ['p2', 'p2', 1],
    [UNMAPPED_PROJECT_VALUE, '', 2],
  ])
  assert.deepEqual(o.authors.map(x => [x.value, x.label, x.count]), [['alice', 'alice', 3], ['bob', 'Bob', 1]])
  assert.deepEqual(o.accounts.map(x => [x.value, x.count]), [['acc1', 4], ['acc2', 1]])
  assert.deepEqual(o.statuses.map(x => [x.value, x.count]), [['not_reviewed', 1], ['reviewed', 1], ['unmapped', 2]])
})

test('effectiveFilters ignores values that are no longer present', () => {
  const o = buildFilterOptions([pr()], statusOf)
  const eff = effectiveFilters(
    { projects: ['p1', 'gone'], authors: ['ghost'], accounts: ['acc1'], statuses: ['running'] },
    o,
  )
  assert.deepEqual(eff, { projects: ['p1'], authors: [], accounts: ['acc1'], statuses: [] })
  assert.equal(hasActiveFilters(eff), true)
  assert.equal(hasActiveFilters(emptyFilters()), false)
})

test('matchesSearch: title, number, owner/repo, author', () => {
  const item = pr({ number: 123, title: 'Fix Login Flow', owner: 'Acme', repo: 'Web', author_login: 'Carol' })
  assert.equal(matchesSearch(item, ''), true)
  assert.equal(matchesSearch(item, 'login flow'), true)
  assert.equal(matchesSearch(item, '#123'), true)
  assert.equal(matchesSearch(item, '123'), true)
  assert.equal(matchesSearch(item, '#12'), false)
  assert.equal(matchesSearch(item, 'acme/web'), true)
  assert.equal(matchesSearch(item, 'carol'), true)
  assert.equal(matchesSearch(item, 'nothing'), false)
})

test('applyFilters ANDs dimensions and ORs values; account matches any reviewer account', () => {
  const a = pr({ project_id: 'p1', author_login: 'alice' })
  const b = pr({ project_id: 'p2', project_name: 'Beta', author_login: 'bob',
    reviewer_accounts: [{ account_id: 'acc1', account_label: 'Work' }, { account_id: 'acc2', account_label: 'P' }] })
  const c = pr({ ...unmapped, author_login: 'bob' })
  const items = [a, b, c]
  const f = (over) => ({ ...emptyFilters(), ...over })
  assert.deepEqual(applyFilters(items, f({ projects: ['p1', UNMAPPED_PROJECT_VALUE] }), '', statusOf), [a, c])
  assert.deepEqual(applyFilters(items, f({ authors: ['bob'], statuses: ['unmapped'] }), '', statusOf), [c])
  assert.deepEqual(applyFilters(items, f({ accounts: ['acc2'] }), '', statusOf), [b])
  assert.deepEqual(applyFilters(items, f({}), String(b.number), statusOf), [b])
})

test('isStale after 7 days; splitLabels caps at 3', () => {
  const now = Date.parse('2026-10-20T00:00:00Z')
  assert.equal(isStale('2026-10-12T23:00:00+00:00', now), true)
  assert.equal(isStale('2026-10-14T00:00:00+00:00', now), false)
  assert.equal(isStale('not a date', now), false)
  assert.deepEqual(splitLabels(['a', 'b', 'c', 'd', 'e']), { shown: ['a', 'b', 'c'], extra: 2 })
  assert.deepEqual(splitLabels(['a']), { shown: ['a'], extra: 0 })
})

test('bulkReviewCandidates: mapped, no run, not reviewing; capped', () => {
  const ok = Array.from({ length: 12 }, () => pr())
  const skip = [
    pr(unmapped),
    pr({ existing_run_id: 'r' }),
    pr({ repo_id: null }),
  ]
  const reviewing = ok[0]
  const res = bulkReviewCandidates([...skip, ...ok], item => item === reviewing)
  assert.equal(res.total, 11)
  assert.equal(res.items.length, 10)
  assert.ok(!res.items.includes(reviewing))
  // Same PR twice (defensive dedup)
  const dup = pr()
  assert.equal(bulkReviewCandidates([dup, { ...dup }], () => false).total, 1)
})

test('canStartReview re-checks the current store entry', () => {
  const item = pr()
  assert.equal(canStartReview(item, false), true)
  assert.equal(canStartReview(undefined, false), false) // gone after a refresh
  assert.equal(canStartReview(item, true), false) // being started elsewhere
  assert.equal(canStartReview({ ...item, existing_run_id: 'run-1' }, false), false) // refreshed: now linked
  assert.equal(canStartReview(pr(unmapped), false), false)
  assert.equal(canStartReview({ ...item, repo_id: null }, false), false)
})

test('ambiguousRepoNames flags repo names shared by several owners', () => {
  const items = [
    pr({ owner: 'Acme', repo: 'Web' }),
    pr({ owner: 'globex', repo: 'web' }),
    pr({ owner: 'acme', repo: 'api' }),
    pr({ owner: 'acme', repo: 'api' }),
  ]
  assert.deepEqual([...ambiguousRepoNames(items)], ['web'])
  assert.equal(ambiguousRepoNames([]).size, 0)
})

test('effectiveAccountCount prefers the configured list, else inbox accounts', () => {
  const configured = [
    { username: 'me', has_pat: true },
    { username: 'me-work', has_pat: true },
    { username: null, has_pat: true }, // not validated
    { username: 'old', has_pat: false }, // no PAT
  ]
  assert.equal(effectiveAccountCount(configured, []), 2)
  const items = [
    pr({ reviewer_accounts: [{ account_id: 'a', account_label: 'A' }, { account_id: 'b', account_label: 'B' }] }),
    pr({ reviewer_accounts: [{ account_id: 'a', account_label: 'A' }] }),
  ]
  assert.equal(effectiveAccountCount(null, items), 2)
  assert.equal(effectiveAccountCount([], items), 2) // list not loaded yet
  assert.equal(effectiveAccountCount(null, [pr()]), 1)
})

test('makeAccountTone: distinct tones by creation order, hash fallback', () => {
  const configured = [
    { id: 'c', created_at: '2026-03-01T00:00:00Z' },
    { id: 'a', created_at: '2026-01-01T00:00:00Z' },
    { id: 'b', created_at: '2026-02-01T00:00:00Z' },
    { id: 'd', created_at: '2026-04-01T00:00:00Z' },
    { id: 'e', created_at: '2026-05-01T00:00:00Z' },
    { id: 'f', created_at: '2026-06-01T00:00:00Z' },
  ]
  const tone = makeAccountTone(configured)
  const tones = ['a', 'b', 'c', 'd', 'e', 'f'].map(tone)
  assert.equal(new Set(tones).size, 6) // no collisions for 6 accounts
  assert.equal(tone('a'), AVATAR_TONES[0]) // oldest first
  assert.equal(tone('c'), AVATAR_TONES[2])
  // Input order (e.g. sorted by label) doesn't matter
  assert.equal(makeAccountTone([...configured].reverse())('b'), tone('b'))
  // Unknown account → stable hash tone
  assert.equal(tone('zzz'), avatarTone('zzz'))
  assert.equal(makeAccountTone([])('a'), avatarTone('a'))
})
