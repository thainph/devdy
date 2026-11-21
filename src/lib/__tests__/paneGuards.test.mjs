// Unit tests for lib/paneGuards.ts (guards of an in-pane navigation in a split).
// Run: node --test src/lib/__tests__/paneGuards.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  callGuard,
  changingRecords,
  paneNavigationGuards,
  paneRecordCopy,
  runGuardQueue,
  sameRecord,
} from '../paneGuards.ts'

const record = (path, extra = {}) => ({
  path,
  leaveGuards: new Set(),
  updateGuards: new Set(),
  components: null,
  instances: {},
  ...extra,
})

test('paneRecordCopy reads through but keeps its own guards and instances', () => {
  const shared = record('/skills', { meta: { tab: true } })
  const own = paneRecordCopy(shared)
  assert.equal(own.path, '/skills')
  assert.deepEqual(own.meta, { tab: true })
  own.updateGuards.add(() => false)
  own.instances.default = {}
  // Nothing leaks into the record the app router uses.
  assert.equal(shared.updateGuards.size, 0)
  assert.deepEqual(shared.instances, {})
  // Still the "same record" as the shared one (RouterLink active state).
  assert.equal(sameRecord(own, shared), true)
  assert.equal(sameRecord(own, paneRecordCopy(shared)), true)
  assert.equal(sameRecord(own, record('/rules')), false)
})

test('changingRecords splits kept and left records', () => {
  const a = record('/a')
  const b = record('/b')
  const c = record('/c')
  assert.deepEqual(changingRecords([a, c], [a, b]), { leaving: [b], updating: [a] })
  assert.deepEqual(changingRecords([paneRecordCopy(a)], [a]), { leaving: [], updating: [a] })
})

test('callGuard reads return values and the next callback', async () => {
  assert.deepEqual(await callGuard(() => undefined), { kind: 'ok' })
  assert.deepEqual(await callGuard(() => true), { kind: 'ok' })
  assert.deepEqual(await callGuard(async () => false), { kind: 'abort' })
  assert.deepEqual(await callGuard(() => '/dashboard'), { kind: 'redirect', to: '/dashboard' })
  const err = new Error('nope')
  assert.deepEqual(await callGuard(() => err), { kind: 'abort', error: err })
  assert.deepEqual(
    await callGuard(() => {
      throw err
    }),
    { kind: 'abort', error: err },
  )
  assert.deepEqual(await callGuard((to, from, next) => setTimeout(() => next(false), 1)), { kind: 'abort' })
  assert.deepEqual(await callGuard((to, from, next) => next()), { kind: 'ok' })
})

test('runGuardQueue stops at the first guard that does not let it through', async () => {
  const ran = []
  const g = (name, out) => async () => {
    ran.push(name)
    return out
  }
  assert.deepEqual(await runGuardQueue([g('a', { kind: 'ok' }), g('b', { kind: 'abort' }), g('c', { kind: 'ok' })]), {
    kind: 'abort',
  })
  assert.deepEqual(ran, ['a', 'b'])
  assert.deepEqual(await runGuardQueue([]), { kind: 'ok' })
})

test('paneNavigationGuards: an update guard returning false cancels a selection change', async () => {
  const skills = paneRecordCopy(record('/skills'))
  let asked = 0
  skills.updateGuards.add(async (to) => {
    asked++
    return to.query.id === 'dirty-ok' ? undefined : false
  })
  const from = { matched: [skills], query: { id: 'a' } }
  const guards = paneNavigationGuards({ matched: [skills], query: { id: 'b' } }, from)
  assert.deepEqual(await runGuardQueue(guards), { kind: 'abort' })
  assert.equal(asked, 1)
  const ok = paneNavigationGuards({ matched: [skills], query: { id: 'dirty-ok' } }, from)
  assert.deepEqual(await runGuardQueue(ok), { kind: 'ok' })
})

test('paneNavigationGuards: leaving runs leave guards (not update guards), component options too', async () => {
  const calls = []
  const instance = { name: 'inst' }
  const component = {
    beforeRouteLeave() {
      calls.push(['option', this])
    },
  }
  const skills = paneRecordCopy(record('/skills', { components: { default: component } }))
  skills.instances.default = instance
  skills.leaveGuards.add(() => {
    calls.push(['leave'])
  })
  skills.updateGuards.add(() => {
    calls.push(['update'])
    return false
  })
  const dashboard = paneRecordCopy(record('/dashboard'))
  const out = await runGuardQueue(paneNavigationGuards({ matched: [dashboard] }, { matched: [skills] }))
  assert.deepEqual(out, { kind: 'ok' })
  assert.deepEqual(calls, [['option', instance], ['leave']])

  // An unmounted component's option guard doesn't run.
  calls.length = 0
  skills.instances.default = undefined
  await runGuardQueue(paneNavigationGuards({ matched: [dashboard] }, { matched: [skills] }))
  assert.deepEqual(calls, [['leave']])
})
