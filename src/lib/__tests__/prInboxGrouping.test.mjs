// Unit tests for lib/prInboxGrouping.ts (PR inbox grouping + bot filter).
// Run: node --test src/lib/__tests__/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  ALL_GROUP_KEY,
  BOTS_GROUP_KEY,
  UNMAPPED_GROUP_KEY,
  groupPrInbox,
  isGroupCollapsedByDefault,
} from '../prInboxGrouping.ts'

let seq = 0
function pr(extra = {}) {
  seq += 1
  return {
    account_id: 'acc',
    account_label: 'Acc',
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
    ...extra,
  }
}

const day = d => `2026-10-${String(d).padStart(2, '0')}T00:00:00+00:00`

test('project mode orders groups by newest PR, unmapped then bots last', () => {
  const items = [
    pr({ project_id: 'p1', project_name: 'Alpha', updated_at: day(2) }),
    pr({ project_id: 'p2', project_name: 'Beta', updated_at: day(5) }),
    pr({ mapped: false, project_id: null, repo_id: null, project_name: null, updated_at: day(9) }),
    pr({ author_login: 'dependabot[bot]', author_is_bot: true, updated_at: day(10) }),
    pr({ project_id: 'p1', project_name: 'Alpha', updated_at: day(7) }),
  ]
  const out = groupPrInbox(items, { groupBy: 'project', hideBots: false })
  assert.deepEqual(out.groups.map(g => g.key), ['project:p1', 'project:p2', `project:${UNMAPPED_GROUP_KEY}`, `project:${BOTS_GROUP_KEY}`])
  assert.deepEqual(out.groups[0].items.map(i => i.updated_at), [day(7), day(2)])
  assert.equal(out.groups[0].label, 'Alpha')
  assert.equal(out.hiddenBotCount, 0)
  assert.equal(out.visibleCount, 5)
})

test('hideBots excludes bot PRs and counts them', () => {
  const items = [
    pr({ updated_at: day(1) }),
    pr({ author_is_bot: true, updated_at: day(3) }),
    pr({ author_is_bot: true, updated_at: day(4) }),
  ]
  const out = groupPrInbox(items, { groupBy: 'repo', hideBots: true })
  assert.deepEqual(out.groups.map(g => g.key), ['repo:acme/web'])
  assert.equal(out.groups[0].items.length, 1)
  assert.equal(out.hiddenBotCount, 2)
  assert.equal(out.visibleCount, 1)
  assert.equal(out.totalCount, 3)
})

test('all-bot inbox with hideBots yields no groups but a hidden count', () => {
  const out = groupPrInbox([pr({ author_is_bot: true })], { groupBy: 'project', hideBots: true })
  assert.deepEqual(out.groups, [])
  assert.equal(out.hiddenBotCount, 1)
  assert.equal(out.visibleCount, 0)
})

test('none mode keeps bots inline when shown, newest first', () => {
  const items = [pr({ updated_at: day(1) }), pr({ author_is_bot: true, updated_at: day(2) })]
  const shown = groupPrInbox(items, { groupBy: 'none', hideBots: false })
  assert.equal(shown.groups.length, 1)
  assert.equal(shown.groups[0].key, `none:${ALL_GROUP_KEY}`)
  assert.deepEqual(shown.groups[0].items.map(i => i.updated_at), [day(2), day(1)])

  const hidden = groupPrInbox(items, { groupBy: 'none', hideBots: true })
  assert.equal(hidden.groups[0].items.length, 1)
  assert.equal(hidden.hiddenBotCount, 1)
})

test('repo and author keys are case-insensitive; author groups carry avatars', () => {
  const items = [
    pr({ owner: 'Acme', repo: 'Web', author_login: 'Alice', updated_at: day(3) }),
    pr({ owner: 'acme', repo: 'web', author_login: 'alice', author_avatar_url: null, updated_at: day(1) }),
    pr({ owner: 'acme', repo: 'api', author_login: 'bob', author_avatar_url: 'https://avatars/bob', updated_at: day(2) }),
  ]
  const byRepo = groupPrInbox(items, { groupBy: 'repo', hideBots: true })
  assert.deepEqual(byRepo.groups.map(g => [g.key, g.label, g.items.length]), [
    ['repo:acme/web', 'Acme/Web', 2],
    ['repo:acme/api', 'acme/api', 1],
  ])
  assert.equal(byRepo.groups[0].avatarUrl, null)

  const byAuthor = groupPrInbox(items, { groupBy: 'author', hideBots: true })
  assert.deepEqual(byAuthor.groups.map(g => [g.key, g.avatarUrl]), [
    ['author:alice', 'https://avatars/alice'],
    ['author:bob', 'https://avatars/bob'],
  ])
})

test('unreviewedCount counts mapped PRs without a run only', () => {
  const items = [
    pr({ existing_run_id: null }),
    pr({ existing_run_id: 'run-1', existing_run_status: 'done' }),
    pr({ mapped: false, project_id: null, repo_id: null, project_name: null }),
  ]
  const out = groupPrInbox(items, { groupBy: 'none', hideBots: true })
  assert.equal(out.groups[0].unreviewedCount, 1)
})

test('blank project names fall back to the project id', () => {
  const items = [
    pr({ project_id: 'p1', project_name: '', updated_at: day(3) }),
    pr({ project_id: 'p2', project_name: '   ', updated_at: day(2) }),
    pr({ project_id: 'p3', project_name: null, updated_at: day(1) }),
  ]
  const out = groupPrInbox(items, { groupBy: 'project', hideBots: true })
  assert.deepEqual(out.groups.map(g => g.label), ['p1', 'p2', 'p3'])
})

test('author logins are trimmed for both key and label', () => {
  const items = [pr({ author_login: ' Alice ', updated_at: day(2) }), pr({ author_login: 'alice', updated_at: day(1) })]
  const out = groupPrInbox(items, { groupBy: 'author', hideBots: true })
  assert.deepEqual(out.groups.map(g => [g.key, g.label, g.items.length]), [['author:alice', 'Alice', 2]])
})

test('bots group key is namespaced per group-by mode', () => {
  const items = [pr(), pr({ author_is_bot: true })]
  const keys = ['project', 'repo', 'author'].map(groupBy =>
    groupPrInbox(items, { groupBy, hideBots: false }).groups.find(g => g.kind === 'bots').key,
  )
  assert.deepEqual(keys, [`project:${BOTS_GROUP_KEY}`, `repo:${BOTS_GROUP_KEY}`, `author:${BOTS_GROUP_KEY}`])
})

test('reviewer_account mode puts a multi-account PR in each group, counted once', () => {
  const both = [{ account_id: 'acc1', account_label: 'Work' }, { account_id: 'acc2', account_label: 'Personal' }]
  const items = [
    pr({ reviewer_accounts: both, updated_at: day(5) }),
    pr({ reviewer_accounts: [{ account_id: 'acc2', account_label: ' ' }], updated_at: day(3) }),
    pr({ updated_at: day(1) }), // older backend: falls back to account_id
    pr({ author_is_bot: true, reviewer_accounts: both, updated_at: day(9) }),
  ]
  const out = groupPrInbox(items, { groupBy: 'reviewer_account', hideBots: false })
  assert.deepEqual(out.groups.map(g => [g.key, g.label, g.items.length]), [
    ['reviewer_account:acc1', 'Work', 1],
    ['reviewer_account:acc2', 'Personal', 2],
    ['reviewer_account:acc', 'Acc', 1],
    [`reviewer_account:${BOTS_GROUP_KEY}`, '', 1],
  ])
  assert.equal(out.visibleCount, 4)
  assert.equal(out.totalCount, 4)
})

test('custom isBot predicate drives hiding', () => {
  const items = [pr({ author_login: 'ci-service' }), pr()]
  const out = groupPrInbox(items, { groupBy: 'author', hideBots: true, isBot: i => i.author_login === 'ci-service' })
  assert.equal(out.hiddenBotCount, 1)
  assert.deepEqual(out.groups.map(g => g.key), ['author:alice'])
})

test('only the bots group is collapsed by default', () => {
  assert.equal(isGroupCollapsedByDefault({ kind: 'bots' }), true)
  assert.equal(isGroupCollapsedByDefault({ kind: 'normal' }), false)
  assert.equal(isGroupCollapsedByDefault({ kind: 'unmapped' }), false)
})
