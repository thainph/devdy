// Unit tests for lib/skillRows.ts (Skills sidebar rows + SKILL.md frontmatter).
// Run: node --test src/lib/__tests__/skillRows.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GROUP_NONE } from '../listGroups.ts'
import {
  FILTER_UNGROUPED,
  buildSkillGroups,
  normalizeFrontmatter,
  parseFrontmatter,
  setFrontmatterField,
  validateFrontmatter,
} from '../skillRows.ts'

const skills = [
  { id: 'a', name: 'git-flow', description: 'Branching model', target: 'both' },
  { id: 'b', name: 'pdf', description: 'Read PDF files', target: 'claude' },
  { id: 'c', name: 'deploy', description: 'Ship to prod', target: 'codex' },
]
const groups = [
  { id: 'g2', name: 'Ops' },
  { id: 'g1', name: 'Dev' },
]
const itemGroups = { a: ['g1', 'g2'], c: ['g2', 'gone'] }
const opts = { search: '', group: '', ungroupedLabel: 'Ungrouped' }

test('buildSkillGroups follows group order, lists multi-group skills in each, ungrouped last', () => {
  const out = buildSkillGroups(skills, itemGroups, groups, opts)
  assert.deepEqual(out.map((g) => [g.key, g.label, g.rows.map((r) => r.id)]), [
    ['g2', 'Ops', ['a', 'c']],
    ['g1', 'Dev', ['a']],
    [GROUP_NONE, 'Ungrouped', ['b']],
  ])
  // Unknown group ids are ignored; the other groups ride along as badges.
  assert.deepEqual(out[0].rows[0].otherGroupIds, ['g1'])
  assert.deepEqual(out[0].rows[1].otherGroupIds, [])
})

test('buildSkillGroups filters by group, ungrouped and search', () => {
  assert.deepEqual(
    buildSkillGroups(skills, itemGroups, groups, { ...opts, group: 'g1' }).map((g) => g.key),
    ['g1'],
  )
  const ungrouped = buildSkillGroups(skills, itemGroups, groups, { ...opts, group: FILTER_UNGROUPED })
  assert.deepEqual(ungrouped.map((g) => g.rows.map((r) => r.id)), [['b']])
  const searched = buildSkillGroups(skills, itemGroups, groups, { ...opts, search: 'PROD ship' })
  assert.deepEqual(searched.map((g) => g.rows.map((r) => r.id)), [['c']])
})

test('parseFrontmatter reads and unquotes values', () => {
  const fm = parseFrontmatter('---\nname: x\ndescription: "say \\"hi\\": now"\n---\nbody')
  assert.deepEqual(fm, { name: 'x', description: 'say "hi": now', target: '' })
  assert.deepEqual(parseFrontmatter('no frontmatter'), { name: '', description: '', target: '' })
})

test('validateFrontmatter reports the first problem', () => {
  assert.equal(validateFrontmatter('---\nname: ok\ndescription: d\n---\n'), null)
  assert.equal(validateFrontmatter('---\nname: ok\n')?.key, 'errFrontmatterMissingClose')
  assert.equal(validateFrontmatter('---\ndescription: d\n---\n')?.key, 'errMissingName')
  assert.equal(validateFrontmatter('---\nname: not a slug\ndescription: d\n---\n')?.key, 'errNameSlug')
  assert.equal(validateFrontmatter('---\nname: ok\n---\n')?.key, 'errMissingDescription')
  assert.equal(validateFrontmatter('---\nname: ok\ndescription: d\ntarget: gpt\n---\n')?.key, 'errTargetInvalid')
  assert.deepEqual(validateFrontmatter('---\n!!bad\nname: ok\n---\n'), { key: 'errInvalidLine', params: { line: '!!bad' } })
})

test('normalizeFrontmatter joins wrapped values and quotes when needed', () => {
  const out = normalizeFrontmatter('---\nname: x\ndescription: first\n  second: part\n---\nbody')
  assert.equal(out.changed, true)
  assert.equal(out.content, '---\nname: x\ndescription: "first second: part"\n---\nbody')
  assert.equal(normalizeFrontmatter('---\nname: x\n---\n').changed, false)
})

test('setFrontmatterField replaces, appends and creates frontmatter', () => {
  const raw = '---\nname: x\ndescription: old\n  wrapped\ntarget: both\n---\n\n# Body'
  assert.equal(
    setFrontmatterField(raw, 'description', 'new: value'),
    '---\nname: x\ndescription: "new: value"\ntarget: both\n---\n\n# Body',
  )
  assert.equal(setFrontmatterField('---\nname: x\n---\n', 'target', 'codex'), '---\nname: x\ntarget: codex\n---\n')
  assert.equal(setFrontmatterField('# Body', 'name', 'y'), '---\nname: y\n---\n\n# Body')
})
