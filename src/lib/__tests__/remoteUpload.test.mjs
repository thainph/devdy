// Unit tests for lib/remoteUploadCore.ts (copying dropped files onto the
// controlled Devdy). Run: node --test src/lib/__tests__/remoteUpload.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { progressPercent, uploadFiles } from '../remoteUploadCore.ts'

/** Local files as strings; the remote side keeps what it received. */
function fakeDeps(files, { failChunk = null } = {}) {
  const remote = new Map()
  const aborted = []
  const progress = []
  let next = 0
  return {
    remote,
    aborted,
    progress,
    deps: {
      chunkBytes: 4,
      stat: async (paths) =>
        paths.map((p) => {
          const name = p.split('/').pop()
          if (p.endsWith('/')) return { path: p, name: p, size: 0, is_dir: true, error: null }
          if (!(p in files)) return { path: p, name, size: 0, is_dir: false, error: 'missing' }
          return { path: p, name, size: files[p].length, is_dir: false, error: null }
        }),
      read: async (path, offset, len) => Buffer.from(files[path].slice(offset, offset + len)).toString('base64'),
      begin: async (name) => {
        const id = `u${++next}`
        remote.set(id, { name, data: '' })
        return { upload_id: id, path: `/b/${id}/${name}` }
      },
      chunk: async (id, offset, b64) => {
        const up = remote.get(id)
        if (failChunk === up.name) throw 'disk full'
        assert.equal(offset, up.data.length)
        up.data += Buffer.from(b64, 'base64').toString()
        return up.data.length
      },
      finish: async (id) => `/b/${id}/${remote.get(id).name}`,
      abort: async (id) => {
        aborted.push(id)
      },
      onProgress: (p) => progress.push(p),
    },
  }
}

test('files are copied in chunks and replaced by their remote paths', async () => {
  const f = fakeDeps({ '/a/one.txt': 'hello world', '/a/empty': '' })
  const out = await uploadFiles(['/a/one.txt', '/a/empty'], f.deps)
  assert.deepEqual(out.paths, ['/b/u1/one.txt', '/b/u2/empty'])
  assert.equal(f.remote.get('u1').data, 'hello world')
  assert.deepEqual(out.failed, [])
  const last = f.progress.at(-1)
  assert.equal(last.done, last.total)
  assert.equal(progressPercent(last), 100)
})

test('folders and unreadable paths are reported, failed uploads aborted', async () => {
  const f = fakeDeps({ '/a/ok': 'x', '/a/bad': 'yyyy' }, { failChunk: 'bad' })
  const out = await uploadFiles(['/a/dir/', '/a/missing', '/a/bad', '/a/ok'], f.deps)
  assert.deepEqual(out.paths, ['/b/u2/ok'])
  assert.deepEqual(out.folders, ['/a/dir/'])
  assert.deepEqual(
    out.failed.map((x) => [x.name, x.error]),
    [
      ['missing', 'missing'],
      ['bad', 'disk full'],
    ],
  )
  assert.deepEqual(f.aborted, ['u1'])
})

test('progress percent', () => {
  assert.equal(progressPercent({ name: '', count: 1, done: 1, total: 3 }), 33)
  assert.equal(progressPercent({ name: '', count: 0, done: 0, total: 0 }), 100)
})
