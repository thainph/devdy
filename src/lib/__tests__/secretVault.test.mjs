// Unit tests for lib/secretVaultCore.ts (secret notes vault helpers).
// Run: node --test src/lib/__tests__/secretVault.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { contentBytes, filterSecretNotes, passwordProblem, vaultError } from '../secretVaultCore.ts'

test('vaultError reads bare codes and the throttle delay', () => {
  assert.deepEqual(vaultError('vault_locked'), { code: 'vault_locked', wait: null })
  assert.deepEqual(vaultError(new Error('wrong_password')), { code: 'wrong_password', wait: null })
  assert.deepEqual(vaultError('vault_throttled:12'), { code: 'vault_throttled', wait: 12 })
  assert.deepEqual(vaultError('keychain write failed: denied'), { code: null, wait: null })
})

test('passwordProblem', () => {
  assert.equal(passwordProblem('short', 'short'), 'short')
  assert.equal(passwordProblem('long enough', 'long enougH'), 'mismatch')
  assert.equal(passwordProblem('long enough', 'long enough'), null)
  assert.equal(passwordProblem('mật khẩu', 'mật khẩu'), null)
})

test('contentBytes counts UTF-8', () => {
  assert.equal(contentBytes('abc'), 3)
  assert.equal(contentBytes('ệ'), 3)
})

test('filterSecretNotes by project and title', () => {
  const notes = [
    { id: '1', title: 'AWS root', project_id: 'p1', position: 0, created_at: '', updated_at: '' },
    { id: '2', title: 'Wifi', project_id: null, position: 1, created_at: '', updated_at: '' },
  ]
  assert.deepEqual(filterSecretNotes(notes, '', '', '__none__').map((n) => n.id), ['1', '2'])
  assert.deepEqual(filterSecretNotes(notes, 'aws', '', '__none__').map((n) => n.id), ['1'])
  assert.deepEqual(filterSecretNotes(notes, '', '__none__', '__none__').map((n) => n.id), ['2'])
  assert.deepEqual(filterSecretNotes(notes, '', 'p1', '__none__').map((n) => n.id), ['1'])
})
