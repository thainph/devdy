// Unit tests for lib/controllerAppearance.ts (phone Controller light / dark mode).
// Run: node --test src/lib/__tests__/controllerAppearance.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DEFAULT_APPEARANCE,
  NO_OVERRIDE,
  parseAppearance,
  parseOverride,
  resolveAppearance,
} from '../controllerAppearance.ts'

const host = { theme: 'light' }

test('no override follows the Host', () => {
  assert.deepEqual(resolveAppearance(host, NO_OVERRIDE), host)
})

test('an override wins over the Host', () => {
  assert.deepEqual(resolveAppearance(host, { theme: 'dark' }), { theme: 'dark' })
})

test('before any Host frame the desktop default applies', () => {
  assert.deepEqual(resolveAppearance(null, NO_OVERRIDE), DEFAULT_APPEARANCE)
  assert.deepEqual(resolveAppearance(null, { theme: 'dark' }), { theme: 'dark' })
})

test('parseAppearance keeps the mode and drops the palette', () => {
  assert.deepEqual(parseAppearance({ theme: 'dark', color_theme: 'nord' }), { theme: 'dark' })
  assert.deepEqual(parseAppearance({ theme: 'neon' }), DEFAULT_APPEARANCE)
  assert.equal(parseAppearance(null), null)
  assert.equal(parseAppearance('dark'), null)
})

test('parseOverride keeps a known mode, ignores an old stored palette', () => {
  assert.deepEqual(parseOverride({ theme: 'system', color_theme: 'midautumn' }), { theme: 'system' })
})

test('parseOverride drops unknown or corrupt values to the Host', () => {
  assert.deepEqual(parseOverride(null), NO_OVERRIDE)
  assert.deepEqual(parseOverride('dark'), NO_OVERRIDE)
  assert.deepEqual(parseOverride({ theme: 'neon', color_theme: 42 }), NO_OVERRIDE)
  assert.deepEqual(parseOverride({ color_theme: 'nord' }), NO_OVERRIDE)
})
