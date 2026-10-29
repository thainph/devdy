// Unit tests for lib/ipc/nativeCore.ts (remote-desktop file dialogs, previews, downloads).
// Run: node --test src/lib/__tests__/remoteNative.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  READ_CHUNK,
  baseName,
  chunkOffsets,
  decodeBase64,
  dialogStart,
  errorStatus,
  formatSize,
  joinPath,
  matchesFilter,
  parentDir,
  resolveRange,
  withFilterExtension,
} from '../ipc/nativeCore.ts'

test('path helpers handle unix and windows paths', () => {
  assert.equal(baseName('/Users/me/a.png'), 'a.png')
  assert.equal(baseName('/Users/me/dir/'), 'dir')
  assert.equal(baseName('C:\\x\\y.txt'), 'y.txt')
  assert.equal(parentDir('/Users/me/a.png'), '/Users/me')
  assert.equal(parentDir('/Users'), '/')
  assert.equal(parentDir('/'), null)
  assert.equal(parentDir('/Users/me/'), '/Users')
  assert.equal(parentDir('C:\\Users\\me'), 'C:\\Users')
  assert.equal(parentDir('C:\\Users'), 'C:\\')
  assert.equal(parentDir('C:\\'), null)
  assert.equal(joinPath('/Users/me', 'a'), '/Users/me/a')
  assert.equal(joinPath('/', 'a'), '/a')
  assert.equal(joinPath('C:\\x', 'a'), 'C:\\x\\a')
})

test('filters match extensions case-insensitively; * and none allow all', () => {
  const img = { name: 'Images', extensions: ['png', 'JPG'] }
  assert.ok(matchesFilter('a.PNG', img))
  assert.ok(matchesFilter('b.jpg', img))
  assert.ok(!matchesFilter('c.gif', img))
  assert.ok(!matchesFilter('png', img))
  assert.ok(matchesFilter('x', { name: 'All', extensions: ['*'] }))
  assert.ok(matchesFilter('x', null))
  assert.equal(withFilterExtension('shot', img), 'shot.png')
  assert.equal(withFilterExtension('shot.jpg', img), 'shot.jpg')
  assert.equal(withFilterExtension('shot', null), 'shot')
})

test('resolveRange: open, bounded, suffix, capped, unsatisfiable', () => {
  assert.deepEqual(resolveRange({ start: 0, end: 1 }, 100), { start: 0, len: 2 })
  assert.deepEqual(resolveRange({ start: 90, end: null }, 100), { start: 90, len: 10 })
  assert.deepEqual(resolveRange({ start: 90, end: 500 }, 100), { start: 90, len: 10 })
  assert.deepEqual(resolveRange({ start: null, end: 30 }, 100), { start: 70, len: 30 })
  assert.deepEqual(resolveRange({ start: null, end: 300 }, 100), { start: 0, len: 100 })
  assert.deepEqual(resolveRange({ start: 0, end: null }, 10 * READ_CHUNK), { start: 0, len: READ_CHUNK })
  assert.equal(resolveRange({ start: 100, end: null }, 100), null)
  assert.equal(resolveRange({ start: null, end: 5 }, 0), null)
})

test('chunk offsets cover the rest of the file', () => {
  assert.deepEqual(chunkOffsets(0, 0), [])
  assert.deepEqual(chunkOffsets(0, 1), [0])
  assert.deepEqual(chunkOffsets(READ_CHUNK, 2 * READ_CHUNK + 1), [READ_CHUNK, 2 * READ_CHUNK])
})

test('read errors map to HTTP statuses', () => {
  assert.equal(errorStatus('not found: /x'), 404)
  assert.equal(errorStatus(new Error('permission denied: /x')), 403)
  assert.equal(errorStatus('is a directory: /x'), 400)
  assert.equal(errorStatus(new Error('remote_timeout')), 502)
})

test('dialogStart: directory, file, missing, relative', () => {
  const dir = { name: 'p', path: '/p', is_dir: true, size: 0, modified: null }
  const file = { ...dir, is_dir: false }
  assert.deepEqual(dialogStart(undefined, null), { dir: null, fileName: '' })
  assert.deepEqual(dialogStart('/p', dir), { dir: '/p', fileName: '' })
  assert.deepEqual(dialogStart('/p/a.json', file), { dir: '/p', fileName: 'a.json' })
  assert.deepEqual(dialogStart('/p/new.json', null), { dir: '/p', fileName: 'new.json' })
  assert.deepEqual(dialogStart('backup.zip', null), { dir: null, fileName: 'backup.zip' })
})

test('decodeBase64 and formatSize', () => {
  assert.deepEqual([...decodeBase64('aGVsbG8=')], [104, 101, 108, 108, 111])
  assert.equal(formatSize(512), '512 B')
  assert.equal(formatSize(1536), '1.5 KB')
  assert.equal(formatSize(20 * 1024 * 1024), '20 MB')
})
