// Unit tests for lib/shortcuts.ts (the About screen's shortcut list).
// Run: node --test src/lib/__tests__/shortcuts.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { SHORTCUT_GROUPS, keyLabel, shortcutGroupsFor } from '../shortcuts.ts'

test('keys read as macOS symbols or their Windows / Linux names', () => {
  assert.equal(keyLabel('Mod', true), '⌘')
  assert.equal(keyLabel('Mod', false), 'Ctrl')
  assert.equal(keyLabel('Shift', true), '⇧')
  assert.equal(keyLabel('K', false), 'K')
})

test('mac-only rows are dropped elsewhere', () => {
  const ids = (mac) => shortcutGroupsFor(mac).flatMap((g) => g.items.map((s) => s.id))
  assert.ok(ids(true).includes('quit'))
  assert.ok(!ids(false).includes('quit'))
})

test('ids are unique within each group', () => {
  for (const g of SHORTCUT_GROUPS) {
    const ids = g.items.map((s) => s.id)
    assert.equal(new Set(ids).size, ids.length, g.id)
  }
})

test('platform-specific combos replace the mac ones elsewhere', () => {
  const find = (mac) => shortcutGroupsFor(mac).flatMap((g) => g.items).find((s) => s.id === 'toggleSidebar')
  assert.deepEqual(find(true).keys, [['Ctrl', 'Mod', 'S']])
  assert.deepEqual(find(false).keys, [['Ctrl', 'Alt', 'S']])
  assert.equal(keyLabel('Alt', true), '⌥')
})
