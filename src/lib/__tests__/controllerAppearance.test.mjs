// Unit tests for lib/controllerAppearance.ts (phone Controller theme override).
// Run: node --test src/lib/__tests__/controllerAppearance.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DEFAULT_APPEARANCE,
  NO_OVERRIDE,
  parseOverride,
  resolveAppearance,
} from '../controllerAppearance.ts'

const host = { theme: 'light', color_theme: 'teal' }

test('no override follows the Host', () => {
  assert.deepEqual(resolveAppearance(host, NO_OVERRIDE), host)
})

test('each field overrides on its own', () => {
  assert.deepEqual(resolveAppearance(host, { theme: 'dark', color_theme: 'host' }), {
    theme: 'dark',
    color_theme: 'teal',
  })
  assert.deepEqual(resolveAppearance(host, { theme: 'host', color_theme: 'rose' }), {
    theme: 'light',
    color_theme: 'rose',
  })
})

test('before any Host frame the desktop defaults apply', () => {
  assert.deepEqual(resolveAppearance(null, NO_OVERRIDE), DEFAULT_APPEARANCE)
  assert.deepEqual(resolveAppearance(null, { theme: 'dark', color_theme: 'host' }), {
    theme: 'dark',
    color_theme: 'default',
  })
})

test('parseOverride keeps known values', () => {
  assert.deepEqual(parseOverride({ theme: 'system', color_theme: 'midautumn' }), {
    theme: 'system',
    color_theme: 'midautumn',
  })
})

test('parseOverride drops unknown or corrupt values to the Host', () => {
  assert.deepEqual(parseOverride(null), NO_OVERRIDE)
  assert.deepEqual(parseOverride('dark'), NO_OVERRIDE)
  assert.deepEqual(parseOverride({ theme: 'neon', color_theme: 42 }), NO_OVERRIDE)
  assert.deepEqual(parseOverride({ theme: 'dark' }), { theme: 'dark', color_theme: 'host' })
})
