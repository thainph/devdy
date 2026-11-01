// Unit tests for lib/systemAccent.ts (macOS accent color → tokens).
// Run: node --test src/lib/__tests__/systemAccent.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { accentForeground, parseRgb, rgbToHslChannels } from '../systemAccent.ts'

test('parses rgb() and rgba()', () => {
  assert.deepEqual(parseRgb('rgb(0, 122, 255)'), [0, 122, 255])
  assert.deepEqual(parseRgb('rgba(255, 149, 0, 1)'), [255, 149, 0])
  assert.deepEqual(parseRgb('rgb(0 122 255)'), [0, 122, 255])
  assert.equal(parseRgb('AccentColor'), null)
})

test('converts to bare HSL channels', () => {
  assert.equal(rgbToHslChannels([0, 122, 255]), '211 100% 50%') // macOS blue
  assert.equal(rgbToHslChannels([255, 255, 255]), '0 0% 100%')
  assert.equal(rgbToHslChannels([128, 128, 128]), '0 0% 50%')
})

test('white text on dark accents, dark text on light ones', () => {
  assert.equal(accentForeground([0, 100, 210]), '0 0% 100%')
  assert.equal(accentForeground([255, 204, 0]), '0 0% 10%') // macOS yellow
})
