// Unit tests for lib/listSidebarView.ts (the Skills / Rules / MCP / Servers
// sidebar's filter menu).
// Run: node --test src/lib/__tests__/listSidebarView.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  clearListFilters,
  isFilteringList,
  listFilterSummary,
  listViewDiffers,
  parseListDisplay,
  pruneListFilters,
} from '../listSidebarView.ts'

const DEFS = [
  { key: 'group', title: 'Group', options: [{ value: 'g1', label: 'Design' }, { value: 'g2', label: 'Ops' }] },
  { key: 'type', title: 'Type', options: [{ value: 'stdio', label: 'stdio' }] },
]
const DEFAULTS = { sort: 'name', grouped: false }

test('isFilteringList / clearListFilters', () => {
  assert.equal(isFilteringList({}), false)
  assert.equal(isFilteringList({ group: '', type: '' }), false)
  assert.equal(isFilteringList({ group: 'g1' }), true)
  assert.deepEqual(clearListFilters({ group: 'g1', type: 'stdio' }), { group: '', type: '' })
})

test('listViewDiffers: filters, sort, folders', () => {
  assert.equal(listViewDiffers({ group: '' }, DEFAULTS, DEFAULTS), false)
  assert.equal(listViewDiffers({ group: 'g1' }, DEFAULTS, DEFAULTS), true)
  assert.equal(listViewDiffers({}, { sort: 'updated', grouped: false }, DEFAULTS), true)
  assert.equal(listViewDiffers({}, { sort: 'name', grouped: true }, DEFAULTS), true)
})

test('listFilterSummary: chosen labels in menu order, unknown values as is', () => {
  assert.deepEqual(listFilterSummary(DEFS, {}), [])
  assert.deepEqual(listFilterSummary(DEFS, { type: 'stdio', group: 'g2' }), ['Ops', 'stdio'])
  assert.deepEqual(listFilterSummary(DEFS, { group: 'gone' }), ['gone'])
  // A section the host no longer shows is not summarised.
  assert.deepEqual(listFilterSummary(DEFS.slice(0, 1), { type: 'stdio' }), [])
})

test('pruneListFilters: drops choices no longer offered', () => {
  assert.equal(pruneListFilters(DEFS, { group: 'g1', type: '' }), null)
  assert.deepEqual(pruneListFilters(DEFS, { group: 'gone', type: 'stdio' }), { group: '', type: 'stdio' })
  assert.deepEqual(pruneListFilters(DEFS.slice(1), { group: 'g1' }), { group: '' })
})

test('parseListDisplay: saved view, legacy order, bad data', () => {
  const sorts = ['name', 'updated']
  assert.deepEqual(parseListDisplay('{"sort":"updated","grouped":true}', null, sorts, DEFAULTS), { sort: 'updated', grouped: true })
  assert.deepEqual(parseListDisplay('{"sort":"nope","grouped":1}', null, sorts, DEFAULTS), DEFAULTS)
  assert.deepEqual(parseListDisplay('not json', null, sorts, DEFAULTS), DEFAULTS)
  assert.deepEqual(parseListDisplay(null, null, sorts, DEFAULTS), DEFAULTS)
  // The old single setting: '' = folders, a sort = flat by it.
  assert.deepEqual(parseListDisplay(null, '', sorts, DEFAULTS), { sort: 'name', grouped: true })
  assert.deepEqual(parseListDisplay(null, 'updated', sorts, DEFAULTS), { sort: 'updated', grouped: false })
  assert.deepEqual(parseListDisplay(null, 'bogus', sorts, DEFAULTS), DEFAULTS)
  // A saved view wins over the legacy one.
  assert.deepEqual(parseListDisplay('{"sort":"name","grouped":false}', '', sorts, DEFAULTS), DEFAULTS)
})
