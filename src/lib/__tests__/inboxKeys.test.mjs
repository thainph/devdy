import { test } from 'node:test'
import assert from 'node:assert/strict'
import { inboxKeyAction, moveSelection } from '../inboxKeys.ts'

test('inboxKeyAction maps vim keys and arrows', () => {
  assert.equal(inboxKeyAction('j'), 'next')
  assert.equal(inboxKeyAction('ArrowDown'), 'next')
  assert.equal(inboxKeyAction('k'), 'prev')
  assert.equal(inboxKeyAction('ArrowUp'), 'prev')
  assert.equal(inboxKeyAction('Enter'), 'primary')
  assert.equal(inboxKeyAction('o'), 'open')
  assert.equal(inboxKeyAction('c'), 'copy')
  assert.equal(inboxKeyAction('x'), null)
})

test('inboxKeyAction ignores modified keys (⌘C stays copy-text)', () => {
  assert.equal(inboxKeyAction('c', { meta: true }), null)
  assert.equal(inboxKeyAction('j', { ctrl: true }), null)
  assert.equal(inboxKeyAction('Enter', { alt: true }), null)
})

test('moveSelection starts at an end and clamps', () => {
  const keys = ['a', 'b', 'c']
  assert.equal(moveSelection(keys, null, 1), 'a')
  assert.equal(moveSelection(keys, null, -1), 'c')
  assert.equal(moveSelection(keys, 'a', 1), 'b')
  assert.equal(moveSelection(keys, 'c', 1), 'c')
  assert.equal(moveSelection(keys, 'a', -1), 'a')
  assert.equal(moveSelection(keys, 'gone', 1), 'a')
  assert.equal(moveSelection([], 'a', 1), null)
})
