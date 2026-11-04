// Unit tests for lib/savedWithoutFolder.ts.
// Run: node --test src/lib/__tests__/savedWithoutFolder.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { savedWithoutFolderNames } from '../savedWithoutFolder.ts'

test('savedWithoutFolderNames keeps order, drops repeats, falls back to the id', () => {
  assert.deepEqual(savedWithoutFolderNames(undefined), [])
  assert.deepEqual(savedWithoutFolderNames([
    { project_id: 'p1', project_name: 'sora' },
    { project_id: 'p2', project_name: '' },
    { project_id: 'p1', project_name: 'sora' },
  ]), ['sora', 'p2'])
})

test('the info messages exist in both locales', () => {
  for (const loc of ['en', 'vi']) {
    const misc = JSON.parse(readFileSync(new URL(`../../i18n/locales/${loc}/misc.json`, import.meta.url), 'utf8'))
    assert.ok(misc.savedWithoutFolder?.one, `${loc}: one`)
    assert.ok(misc.savedWithoutFolder?.many, `${loc}: many`)
  }
})
