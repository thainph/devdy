// Unit tests for lib/runRef.ts (issue/PR identity line in the History list).
// Run: node --test src/lib/__tests__/runRef.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { repoShortName, runRefLine } from '../runRef.ts'

const pr = { run_type: 'review_pr', ref_number: 42, ref_author: 'octo' }

test('repo, number and author', () => {
  assert.equal(runRefLine(pr, 'acme/web'), 'acme/web #42 · @octo')
})

test('sessions have no identity line', () => {
  assert.equal(runRefLine({ run_type: 'session', ref_number: null }, 'acme/web'), null)
})

test('skips the author the title already shows', () => {
  assert.equal(runRefLine(pr, 'acme/web', 'PR #42 · @octo · Fix login'), 'acme/web #42')
  assert.equal(runRefLine(pr, 'acme/web', 'PR #42 · @octopus'), 'acme/web #42 · @octo')
})

test('leaves out what is unknown', () => {
  assert.equal(runRefLine({ ...pr, ref_author: null }, 'acme/web'), 'acme/web #42')
  assert.equal(runRefLine(pr, null), '#42 · @octo')
  assert.equal(runRefLine({ run_type: 'analyze_issue', ref_number: null }, null), null)
})

test('short repo name is the last path segment', () => {
  assert.equal(repoShortName('acme/web'), 'web')
  assert.equal(repoShortName('group/sub/api'), 'api')
  assert.equal(repoShortName('solo'), 'solo')
})
