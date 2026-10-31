// Unit tests for lib/rulesRows.ts (Rules sidebar filtering/grouping + frontmatter).
// Run: node --test src/lib/__tests__/rulesRows.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GROUP_NONE } from '../listGroups.ts'
import {
  UNGROUPED,
  buildRuleGroups,
  matchesEngine,
  parseFrontmatter,
  setFrontmatterField,
  validateFrontmatter,
} from '../rulesRows.ts'

const rules = [
  { id: 'a', name: 'git-rule', description: 'Commit messages', target: 'both' },
  { id: 'b', name: 'lang', description: 'Reply in Vietnamese', target: 'claude' },
  { id: 'c', name: 'ci', description: 'Actions convention', target: 'codex' },
]
const groups = [
  { id: 'g2', name: 'Workflow' },
  { id: 'g1', name: 'Basics' },
]
const itemGroups = { a: ['g1', 'g2'], b: ['g1', 'gone'] }

function build(over = {}) {
  return buildRuleGroups(rules, {
    groups,
    itemGroups,
    search: '',
    engine: '',
    groupFilter: '',
    noneLabel: 'Ungrouped',
    ...over,
  })
}

function shape(groupsOut) {
  return groupsOut.map((g) => [g.key, g.label, g.rows.map((r) => r.id)])
}

test('matchesEngine treats both as either engine', () => {
  assert.equal(matchesEngine('both', 'codex'), true)
  assert.equal(matchesEngine('claude', 'codex'), false)
  assert.equal(matchesEngine('codex', ''), true)
})

test('rules sit under every group, in group order, ungrouped last', () => {
  const out = build()
  assert.deepEqual(shape(out), [
    ['g2', 'Workflow', ['a']],
    ['g1', 'Basics', ['a', 'b']],
    [GROUP_NONE, 'Ungrouped', ['c']],
  ])
  // Stale group ids are ignored; the other groups become badges.
  assert.deepEqual(out[1].rows.map((r) => r.otherGroupIds), [['g2'], []])
})

test('group filter keeps one folder', () => {
  assert.deepEqual(shape(build({ groupFilter: 'g2' })), [['g2', 'Workflow', ['a']]])
  assert.deepEqual(shape(build({ groupFilter: UNGROUPED })), [[GROUP_NONE, 'Ungrouped', ['c']]])
})

test('engine filter and search (incl. content) narrow the rows', () => {
  assert.deepEqual(shape(build({ engine: 'claude' })), [
    ['g2', 'Workflow', ['a']],
    ['g1', 'Basics', ['a', 'b']],
  ])
  assert.deepEqual(shape(build({ search: 'actions' })), [[GROUP_NONE, 'Ungrouped', ['c']]])
  assert.deepEqual(shape(build({ search: 'squash', contents: { b: '- Squash merge only' } })), [
    ['g1', 'Basics', ['b']],
  ])
})

test('frontmatter parse + validate', () => {
  const raw = '---\nname: "my-rule"\ndescription: Does things\ncodex_mode: skill\n---\n\nBody\n'
  assert.deepEqual(parseFrontmatter(raw), {
    name: 'my-rule',
    description: 'Does things',
    target: '',
    codexMode: 'skill',
  })
  assert.equal(validateFrontmatter(raw), null)
  assert.equal(validateFrontmatter('no frontmatter'), 'errMissingName')
  assert.equal(validateFrontmatter('---\nname: my rule\n---\n'), 'errNameSlug')
  assert.equal(validateFrontmatter('---\nname: r\n---\n'), 'errMissingDescription')
  assert.equal(validateFrontmatter('---\nname: r\ndescription: d\ntarget: gpt\n---\n'), 'errTargetInvalid')
  assert.equal(validateFrontmatter('---\nname: r\ndescription: d\ncodex_mode: x\n---\n'), 'errCodexModeInvalid')
})

test('setFrontmatterField rewrites, appends and drops lines', () => {
  const raw = '---\nname: r\ndescription: d\n---\n\nBody\n'
  assert.equal(setFrontmatterField(raw, 'name', 'x'), '---\nname: x\ndescription: d\n---\n\nBody\n')
  const added = setFrontmatterField(raw, 'codex_mode', 'skill')
  assert.equal(added, '---\nname: r\ndescription: d\ncodex_mode: skill\n---\n\nBody\n')
  assert.equal(setFrontmatterField(added, 'codex_mode', null), raw)
  assert.equal(setFrontmatterField('Body', 'name', 'x'), 'Body')
})
