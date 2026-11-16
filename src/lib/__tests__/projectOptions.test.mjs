// Unit tests for lib/projectOptions.ts (the shared order of project pickers).
// Run: node --test src/lib/__tests__/projectOptions.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { projectOptions, projectOptionsForIds, sortProjectsByName } from '../projectOptions.ts'

test('sortProjectsByName: case-insensitive, numeric-aware, id tie-break, input untouched', () => {
  const input = [
    { id: 'c', name: 'project 10' },
    { id: 'b', name: 'Alpha' },
    { id: 'a', name: 'Project 2' },
    { id: 'e', name: 'alpha' },
    { id: 'd', name: 'beta' },
  ]
  const before = input.map((p) => p.id)
  assert.deepEqual(sortProjectsByName(input).map((p) => p.id), ['b', 'e', 'd', 'a', 'c'])
  assert.deepEqual(input.map((p) => p.id), before)
})

test('projectOptions maps to { value, label } in name order', () => {
  assert.deepEqual(projectOptions([{ id: '2', name: 'Zeta' }, { id: '1', name: 'Devdy' }]), [
    { value: '1', label: 'Devdy' },
    { value: '2', label: 'Zeta' },
  ])
})

test('projectOptionsForIds: dedupes, falls back to the id, sorts by name', () => {
  const names = { p1: 'Zeta', p2: 'Alpha' }
  assert.deepEqual(projectOptionsForIds(['p1', 'gone', 'p2', 'p1'], (id) => names[id]), [
    { value: 'p2', label: 'Alpha' },
    { value: 'gone', label: 'gone' },
    { value: 'p1', label: 'Zeta' },
  ])
})
