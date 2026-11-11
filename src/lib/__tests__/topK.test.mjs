// Unit tests for lib/topK.ts and lib/reconcileRows.ts (perf helpers used by the
// @-mention picker and the runs store).
// Run: node --test src/lib/__tests__/topK.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { topKByScore } from '../topK.ts'
import { reconcileById, shallowEqualRecord } from '../reconcileRows.ts'

function naive(items, score, k) {
  return items
    .map((item) => ({ item, s: score(item) }))
    .filter((x) => x.s >= 0)
    .sort((a, b) => b.s - a.s)
    .slice(0, k)
    .map((x) => x.item)
}

test('topKByScore matches a stable full sort, ties in source order', () => {
  const items = Array.from({ length: 500 }, (_, i) => ({ i, s: (i * 7919) % 13 - 2 }))
  for (const k of [0, 1, 5, 50, 1000]) {
    assert.deepEqual(topKByScore(items, (x) => x.s, k), naive(items, (x) => x.s, k))
  }
})

test('topKByScore drops negative scores', () => {
  assert.deepEqual(topKByScore([1, -1, 3, -5], (x) => x, 10), [3, 1])
})

test('shallowEqualRecord compares own keys by identity', () => {
  assert.equal(shallowEqualRecord({ a: 1, b: null }, { a: 1, b: null }), true)
  assert.equal(shallowEqualRecord({ a: 1 }, { a: 1, b: undefined }), false)
  assert.equal(shallowEqualRecord({ a: 1 }, { a: 2 }), false)
})

test('reconcileById returns null when ids/order are unchanged and patches in place', () => {
  const a = { id: 'a', t: 1 }
  const b = { id: 'b', t: 2 }
  const cur = [a, b]
  assert.equal(reconcileById(cur, [{ id: 'a', t: 1 }, { id: 'b', t: 3 }]), null)
  assert.equal(b.t, 3)
  assert.equal(cur[1], b)
})

test('reconcileById reuses existing rows when membership changes', () => {
  const a = { id: 'a', t: 1 }
  const b = { id: 'b', t: 2 }
  const c = { id: 'c', t: 9 }
  const out = reconcileById([a, b], [c, { id: 'a', t: 1 }])
  assert.deepEqual(out.map((r) => r.id), ['c', 'a'])
  assert.equal(out[0], c)
  assert.equal(out[1], a)
  // Reordering alone also yields a new array.
  assert.deepEqual(reconcileById([a, b], [{ ...b }, { ...a }]), [b, a])
})
