// Pure-function unit tests for the ssh/scp shim (AC-307).
// Run with: node --test sidecar-proxy/ssh-shim.test.mjs
import { test } from 'node:test'
import assert from 'node:assert'
import { buildArgs } from './ssh-shim.mjs'

test('buildArgs prepends -F <config> when config is set', () => {
  assert.deepEqual(
    buildArgs('/tmp/run/config', ['web', 'uname -a']),
    ['-F', '/tmp/run/config', 'web', 'uname -a'],
  )
})

test('buildArgs passes argv through unchanged when config is empty', () => {
  // BR-304: no config → behave exactly like the real ssh.
  assert.deepEqual(buildArgs('', ['web', 'ls']), ['web', 'ls'])
  assert.deepEqual(buildArgs(undefined, ['web', 'ls']), ['web', 'ls'])
})

test('buildArgs -F lands at the FRONT so the config wins', () => {
  const out = buildArgs('/cfg', ['-p', '2222', 'host'])
  assert.equal(out[0], '-F')
  assert.equal(out[1], '/cfg')
})

test('buildArgs tolerates a missing argv', () => {
  assert.deepEqual(buildArgs('/cfg', undefined), ['-F', '/cfg'])
  assert.deepEqual(buildArgs('', undefined), [])
})
