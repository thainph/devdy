// Unit tests for lib/deviceSync.ts.
// Run: node --test src/lib/__tests__/deviceSync.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  MIRROR_DELETE_KEYS,
  SYNC_CATEGORY_KEYS,
  SYNC_GROUPS,
  compactReport,
  deviceLabels,
  groupState,
  needsRelogin,
  syncDay,
  SYNC_ERROR_CODES,
  defaultCategories,
  deviceLabel,
  groupWarnings,
  isDeviceGone,
  mirrorDeletions,
  normalizeWarning,
  progressPercent,
  reportRows,
  reportTotals,
  selectedCategories,
  syncErrorCode,
} from '../deviceSync.ts'

test('defaultCategories ticks every category with mirror off', () => {
  const c = defaultCategories()
  assert.deepEqual(Object.keys(c), [...SYNC_CATEGORY_KEYS, 'mirror'])
  assert.equal(c.mirror, false)
  assert.ok(SYNC_CATEGORY_KEYS.every(k => c[k] === true))
  assert.deepEqual(selectedCategories(c), [...SYNC_CATEGORY_KEYS])
  assert.deepEqual(mirrorDeletions(c), [])
})

test('mirrorDeletions lists only ticked deletable categories', () => {
  const c = defaultCategories()
  c.mirror = true
  assert.deepEqual(mirrorDeletions(c), [...MIRROR_DELETE_KEYS])
  for (const k of ['secretNotes', 'slackThreads', 'projects', 'settings']) {
    assert.ok(!MIRROR_DELETE_KEYS.includes(k), k)
  }
  c.notes = false
  c.accounts = false
  assert.deepEqual(mirrorDeletions(c), ['todos', 'skillsRules', 'mcpServers', 'vpsServers'])
  c.mirror = false
  assert.deepEqual(mirrorDeletions(c), [])
})

test('selectedCategories keeps category order', () => {
  const c = defaultCategories()
  for (const k of SYNC_CATEGORY_KEYS) c[k] = false
  c.accounts = true
  c.todos = true
  assert.deepEqual(selectedCategories(c), ['todos', 'accounts'])
})

test('syncErrorCode recognises every backend code, anything else is null', () => {
  for (const code of SYNC_ERROR_CODES) assert.equal(syncErrorCode(code), code)
  assert.equal(syncErrorCode(' device_offline '), 'device_offline')
  assert.equal(syncErrorCode('relay closed'), null)
  assert.equal(syncErrorCode(new Error('wrong_password')), null)
  assert.equal(syncErrorCode(undefined), null)
})

test('every backend error code is translated in both locales', () => {
  for (const code of ['bad_request', 'bundle_corrupt', 'bundle_unsupported_version']) {
    assert.ok(SYNC_ERROR_CODES.includes(code), code)
  }
  for (const locale of ['en', 'vi']) {
    const url = new URL(`../../i18n/locales/${locale}/deviceSync.json`, import.meta.url)
    const errors = JSON.parse(readFileSync(url, 'utf8')).errors
    for (const code of SYNC_ERROR_CODES) assert.ok(errors[code], `${locale}: ${code}`)
  }
})

test('isDeviceGone only for offline / disconnected', () => {
  assert.ok(isDeviceGone('device_offline'))
  assert.ok(isDeviceGone('device_disconnected'))
  assert.ok(!isDeviceGone('timeout'))
  assert.ok(!isDeviceGone(null))
})

test('deviceLabel falls back to the device id when the alias is empty', () => {
  assert.equal(deviceLabel({ alias: 'mac-mini', deviceId: 'abcdef0123456789' }), 'mac-mini')
  assert.equal(deviceLabel({ alias: '  ', deviceId: 'abcdef0123456789' }), 'Devdy · abcdef01')
  assert.equal(deviceLabel({ alias: '', deviceId: 'abc' }), 'Devdy · abc')
})

test('progressPercent follows the phase', () => {
  assert.equal(progressPercent(null), 0)
  assert.equal(progressPercent({ phase: 'connecting', received: 0, total: 0 }), 0)
  assert.equal(progressPercent({ phase: 'downloading', received: 5, total: 0 }), 0)
  assert.equal(progressPercent({ phase: 'downloading', received: 1, total: 4 }), 25)
  assert.equal(progressPercent({ phase: 'downloading', received: 9, total: 4 }), 100)
  assert.equal(progressPercent({ phase: 'applying', received: 4, total: 4 }), 100)
  assert.equal(progressPercent({ phase: 'done', received: 0, total: 0 }), 100)
})

test('reportRows orders categories first, extras sorted, zero-filled', () => {
  const rows = reportRows({
    counts: {
      zeta: { created: 1 },
      accounts: { created: 1, updated: 0, unchanged: 2, deleted: 0, skipped: 3 },
      todos: { created: 2, updated: 1, unchanged: 0, deleted: 4, skipped: 0 },
      alpha: { updated: 5 },
    },
    warnings: [],
  })
  assert.deepEqual(rows.map(r => r.key), ['todos', 'accounts', 'alpha', 'zeta'])
  assert.deepEqual(rows[2].counts, { created: 0, updated: 5, unchanged: 0, deleted: 0, skipped: 0 })
})

test('reportTotals sums every row', () => {
  const total = reportTotals({
    counts: {
      todos: { created: 2, updated: 1, unchanged: 0, deleted: 4, skipped: 0 },
      notes: { created: 1, updated: 1, unchanged: 1, deleted: 0, skipped: 1 },
    },
    warnings: [],
  })
  assert.deepEqual(total, { created: 3, updated: 2, unchanged: 1, deleted: 4, skipped: 1 })
  assert.deepEqual(reportTotals({ counts: {}, warnings: [] }), { created: 0, updated: 0, unchanged: 0, deleted: 0, skipped: 0 })
})

test('groupWarnings: problems first, then folders / links / mirror, repeats dropped', () => {
  const folder = (project) => ({ code: 'project_folder_missing', params: { project, path: `/x/${project}` } })
  const groups = groupWarnings([
    { code: 'category_empty_no_mirror', params: { section: 'skills' } },
    folder('pms'),
    { code: 'link_target_missing', params: { project: 'pms', kind: 'vpsServer', name: 'prod' } },
    folder('sora'),
    folder('pms'),
    { code: 'item_failed', params: { section: 'notes', name: 'n', error: 'e' } },
    'legacy text',
    'legacy text',
  ])
  assert.deepEqual(groups.map(g => [g.group, g.items.length]), [
    ['problems', 2], ['folders', 2], ['links', 1], ['mirror', 1],
  ])
  assert.deepEqual(groups[0].items[1], { code: 'other', params: { message: 'legacy text' } })
  assert.deepEqual(groups[1].items.map(w => w.params.project), ['pms', 'sora'])
  // Param order doesn't make a repeat look new.
  const same = groupWarnings([
    { code: 'other', params: { a: '1', b: '2' } },
    { code: 'other', params: { b: '2', a: '1' } },
  ])
  assert.equal(same[0].items.length, 1)
  assert.deepEqual(groupWarnings([]), [])
})

test('normalizeWarning fills missing params', () => {
  assert.deepEqual(normalizeWarning({ code: 'x' }), { code: 'x', params: {} })
  assert.deepEqual(normalizeWarning('m'), { code: 'other', params: { message: 'm' } })
})

test('deviceLabels tells devices with the same name apart', () => {
  const labels = deviceLabels([
    { deviceId: 'aaaaaaaa-1111', alias: 'macbook' },
    { deviceId: 'bbbbbbbb-2222', alias: 'macbook' },
    { deviceId: 'cccccccc-3333', alias: 'mac-mini' },
    { deviceId: 'dddddddd-4444', alias: '' },
  ])
  assert.deepEqual(labels, {
    'aaaaaaaa-1111': 'macbook · aaaaaaaa',
    'bbbbbbbb-2222': 'macbook · bbbbbbbb',
    'cccccccc-3333': 'mac-mini',
    'dddddddd-4444': 'Devdy · dddddddd',
  })
})

test('SYNC_GROUPS covers every category exactly once', () => {
  const all = SYNC_GROUPS.flatMap(g => g.items)
  assert.deepEqual([...all].sort(), [...SYNC_CATEGORY_KEYS].sort())
  assert.equal(new Set(all).size, all.length)
  for (const locale of ['en', 'vi']) {
    const url = new URL(`../../i18n/locales/${locale}/deviceSync.json`, import.meta.url)
    const groups = JSON.parse(readFileSync(url, 'utf8')).groups
    for (const g of SYNC_GROUPS) assert.ok(groups[g.key], `${locale}: groups.${g.key}`)
  }
})

test('groupState: all, none or mixed', () => {
  const c = defaultCategories()
  const items = ['settings', 'mcpServers', 'vpsServers', 'accounts']
  assert.equal(groupState(c, items), true)
  c.accounts = false
  assert.equal(groupState(c, items), 'mixed')
  for (const k of items) c[k] = false
  assert.equal(groupState(c, items), false)
})

test('compactReport keeps rows and columns where something happened', () => {
  const z = { created: 0, updated: 0, unchanged: 0, deleted: 0, skipped: 0 }
  const { rows, columns, hidden } = compactReport({
    counts: {
      todos: { ...z, created: 3, updated: 14, unchanged: 2 },
      notes: { ...z, unchanged: 7 },
      projects: { ...z, updated: 2, skipped: 1 },
      secretNotes: { ...z },
    },
    warnings: [],
  })
  assert.deepEqual(rows.map(r => r.key), ['todos', 'projects'])
  assert.deepEqual(columns, ['created', 'updated', 'skipped'])
  assert.equal(hidden, 2)
  assert.deepEqual(compactReport({ counts: {}, warnings: [] }), { rows: [], columns: [], hidden: 0 })
})

test('needsRelogin only when a Claude / AWS / Azure account came in', () => {
  const z = { created: 0, updated: 0, unchanged: 0, deleted: 0, skipped: 0 }
  assert.equal(needsRelogin({ counts: { githubAccounts: { ...z, created: 1 } }, warnings: [] }), false)
  assert.equal(needsRelogin({ counts: { claudeAccounts: { ...z, unchanged: 2 } }, warnings: [] }), false)
  assert.equal(needsRelogin({ counts: { awsAccounts: { ...z, updated: 1 } }, warnings: [] }), true)
})

test('syncDay buckets a time relative to now', () => {
  const now = new Date(2026, 10, 4, 10, 0)
  assert.equal(syncDay(new Date(2026, 10, 4, 0, 5).toISOString(), now), 'today')
  assert.equal(syncDay(new Date(2026, 10, 3, 14, 32).toISOString(), now), 'yesterday')
  assert.equal(syncDay(new Date(2026, 10, 2, 23, 59).toISOString(), now), 'earlier')
  assert.equal(syncDay('not a date', now), null)
})
