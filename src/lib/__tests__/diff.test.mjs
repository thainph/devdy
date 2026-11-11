// Unit tests for lib/diff.ts: the count-only `diffStats` (used by Edit header
// badges) must agree with the counts `computeDiff` reports for the full diff.
// Run: node --test src/lib/__tests__/diff.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'

const { computeDiff, diffStats } = await import('../diff.ts')

// Deterministic PRNG so failures reproduce.
function rng(seed) {
  let s = seed >>> 0
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32)
}

function randomText(rand, lines, alphabet) {
  const out = []
  for (let i = 0; i < lines; i++) out.push(alphabet[Math.floor(rand() * alphabet.length)])
  return out.join('\n') + (rand() < 0.5 ? '\n' : '')
}

test('diffStats matches computeDiff counts on random edits', () => {
  const rand = rng(42)
  const alphabet = ['a', 'b', 'c', 'd', '', '}', 'return x']
  for (let k = 0; k < 500; k++) {
    const before = randomText(rand, Math.floor(rand() * 30), alphabet)
    const after = randomText(rand, Math.floor(rand() * 30), alphabet)
    const full = computeDiff(before, after, { fold: false })
    assert.deepEqual(diffStats(before, after), { added: full.added, removed: full.removed }, `case ${k}`)
  }
})

test('diffStats on shared head/tail and edge cases', () => {
  const head = Array.from({ length: 200 }, (_, i) => `line ${i}`).join('\n')
  const before = `${head}\nold\n${head}`
  const after = `${head}\nnew one\nnew two\n${head}`
  assert.deepEqual(diffStats(before, after), { added: 2, removed: 1 })
  assert.deepEqual(diffStats('', 'a\nb'), { added: 2, removed: 0 })
  assert.deepEqual(diffStats('a\nb\n', ''), { added: 0, removed: 2 })
  assert.deepEqual(diffStats('same\n', 'same'), { added: 0, removed: 0 })
})

test('computeDiff rows are unchanged by the typed-array table', () => {
  const d = computeDiff('a\nb\nc\nd', 'a\nx\nc\nd\ne', { fold: false })
  assert.deepEqual(d.rows.map((r) => `${r.type}:${r.text}`), ['ctx:a', 'del:b', 'add:x', 'ctx:c', 'ctx:d', 'add:e'])
  assert.equal(d.added, 2)
  assert.equal(d.removed, 1)
})
