// Unit tests for lib/fileTransferCore.ts (remote-desktop File Transfer window).
// Run: node --test src/lib/__tests__/fileTransferCore.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import {
  ByteBudget,
  CanceledError,
  MAX_IN_FLIGHT_BYTES,
  TRANSFER_CHUNK,
  ConflictResolver,
  TransferQueue,
  base64Bytes,
  breadcrumbs,
  copyFile,
  freeName,
  isInside,
  itemPercent,
  joinRel,
  numberedName,
  planFolder,
  planTransfer,
  queueProgress,
  relParent,
  wireBytes,
  writeConflict,
} from '../fileTransferCore.ts'

test('relative paths join with the destination separator', () => {
  assert.equal(joinRel('/Users/me', 'a/b/c.txt'), '/Users/me/a/b/c.txt')
  assert.equal(joinRel('/Users/me/', 'a'), '/Users/me/a')
  assert.equal(joinRel('C:\\Users\\me', 'a/b/c.txt'), 'C:\\Users\\me\\a\\b\\c.txt')
  assert.equal(joinRel('C:\\', 'a/b'), 'C:\\a\\b')
  assert.equal(joinRel('/x', ''), '/x')
  assert.equal(relParent('a/b/c.txt'), 'a/b')
  assert.equal(relParent('c.txt'), '')
})

test('breadcrumbs split unix and windows paths', () => {
  assert.deepEqual(breadcrumbs('/Users/me'), [
    { label: '/', path: '/' },
    { label: 'Users', path: '/Users' },
    { label: 'me', path: '/Users/me' },
  ])
  assert.deepEqual(breadcrumbs('/'), [{ label: '/', path: '/' }])
  assert.deepEqual(breadcrumbs('C:\\a\\b'), [
    { label: 'C:\\', path: 'C:\\' },
    { label: 'a', path: 'C:\\a' },
    { label: 'b', path: 'C:\\a\\b' },
  ])
  assert.deepEqual(breadcrumbs(''), [])
})

test('numbered names mirror the backend', () => {
  assert.equal(numberedName('a.tar.gz', 2), 'a.tar (2).gz')
  assert.equal(numberedName('README', 1), 'README (1)')
  assert.equal(numberedName('.env', 1), '.env (1)')
  assert.equal(numberedName('x.png', 0), 'x.png')
  assert.equal(freeName('dir', ['dir', 'dir (1)']), 'dir (2)')
  assert.equal(freeName('new', ['dir']), 'new')
})

test('isInside matches the folder itself and its descendants only', () => {
  assert.ok(isInside('/a/b', '/a/b'))
  assert.ok(isInside('/a/b/c', '/a/b/'))
  assert.ok(!isInside('/a/bc', '/a/b'))
  assert.ok(isInside('C:\\a\\b', 'C:\\a'))
})

test('the conflict resolver remembers "apply to all"', async () => {
  const asked = []
  const answers = [
    { choice: 'rename', applyToAll: false },
    { choice: 'skip', applyToAll: true },
  ]
  const r = new ConflictResolver(async (name) => {
    asked.push(name)
    return answers.shift()
  })
  assert.equal(await r.resolve('a', false), 'rename')
  assert.equal(await r.resolve('b', true), 'skip')
  assert.equal(await r.resolve('c', false), 'skip')
  assert.deepEqual(asked, ['a', 'b'])
  assert.equal(await new ConflictResolver(async () => null).resolve('x', false), null)
  assert.equal(writeConflict('overwrite'), 'overwrite')
  assert.equal(writeConflict('rename'), 'rename')
  assert.equal(writeConflict(null), 'fail')
})

const walk = {
  entries: [
    { rel: 'a.txt', is_dir: false, size: 5 },
    { rel: 'sub', is_dir: true, size: 0 },
    { rel: 'sub/b.bin', is_dir: false, size: 7 },
  ],
  total_bytes: 12,
  truncated: false,
  skipped: 0,
}

test('a folder plan creates the tree, then copies its files', () => {
  const { dirs, jobs } = planFolder('down', '/b/proj', 'C:\\dl\\proj', walk, 'fail')
  assert.deepEqual(dirs, ['C:\\dl\\proj', 'C:\\dl\\proj\\sub'])
  assert.deepEqual(
    jobs.map((j) => [j.src, j.destDir, j.name, j.size, j.label]),
    [
      ['/b/proj/a.txt', 'C:\\dl\\proj', 'a.txt', 5, 'proj/a.txt'],
      ['/b/proj/sub/b.bin', 'C:\\dl\\proj\\sub', 'b.bin', 7, 'proj/sub/b.bin'],
    ],
  )
})

function entry(name, is_dir, size = 0) {
  return { name, path: `/src/${name}`, is_dir, size, modified: null }
}

function planDeps(existing, answers) {
  const made = []
  return {
    made,
    deps: {
      walk: async (path) => (path.endsWith('huge') ? { ...walk, truncated: true } : walk),
      list: async () => existing.map((n) => entry(n, false)),
      mkdir: async (p) => made.push(p),
      resolver: new ConflictResolver(async () => answers.shift() ?? null),
    },
  }
}

test('planning asks on clashes: overwrite, rename (keep both) and skip', async () => {
  const { deps, made } = planDeps(['f.txt', 'proj', 'g'], [
    { choice: 'overwrite', applyToAll: false },
    { choice: 'rename', applyToAll: false },
    { choice: 'skip', applyToAll: false },
  ])
  const plan = await planTransfer(
    'up',
    [entry('f.txt', false, 3), entry('proj', true), entry('g', false, 1), entry('new.txt', false, 2)],
    '/dest',
    deps,
  )
  assert.equal(plan.canceled, false)
  assert.deepEqual(plan.skipped, ['g'])
  assert.deepEqual(
    plan.jobs.map((j) => [j.label, j.destDir, j.conflict]),
    [
      ['f.txt', '/dest', 'overwrite'],
      ['proj/a.txt', '/dest/proj (1)', 'fail'],
      ['proj/sub/b.bin', '/dest/proj (1)/sub', 'fail'],
      ['new.txt', '/dest', 'fail'],
    ],
  )
  assert.deepEqual(made, ['/dest/proj (1)', '/dest/proj (1)/sub'])
})

test('a merged folder overwrites inside; a cancelled prompt queues nothing; huge trees fail', async () => {
  const merged = planDeps(['proj'], [{ choice: 'overwrite', applyToAll: false }])
  const plan = await planTransfer('down', [entry('proj', true)], '/dest', merged.deps)
  assert.deepEqual(plan.jobs.map((j) => j.conflict), ['overwrite', 'overwrite'])
  assert.deepEqual(merged.made, ['/dest/proj', '/dest/proj/sub'])

  const cancel = planDeps(['a'], [])
  const none = await planTransfer('up', [entry('b', false), entry('a', false)], '/dest', cancel.deps)
  assert.equal(none.canceled, true)
  assert.equal(none.jobs.length, 0)

  const huge = planDeps([], [])
  const failed = await planTransfer('up', [entry('huge', true), entry('ok', false, 1)], '/dest', huge.deps)
  assert.deepEqual(failed.failed, [{ name: 'huge', error: 'too_many_entries' }])
  assert.deepEqual(failed.jobs.map((j) => j.name), ['ok'])
})

/** An in-memory pair of machines: `src` files are read, `dest` files written. */
function fakeIo({ src = {}, failWriteAt = -1, corrupt = false, delay = 0 } = {}) {
  const dest = {}
  const writes = new Map()
  const log = { aborted: [], begun: 0, maxOpen: 0 }
  let nextId = 0
  const tick = () => new Promise((r) => setTimeout(r, delay))
  const io = {
    begin: async (destDir, name, size, conflict) => {
      await tick()
      const id = `w${nextId++}`
      writes.set(id, { path: `${destDir}/${name}`, size, conflict, bytes: Buffer.alloc(0) })
      log.begun++
      log.maxOpen = Math.max(log.maxOpen, writes.size)
      return id
    },
    read: async (path, offset, len) => {
      await tick()
      return src[path].subarray(offset, offset + len).toString('base64')
    },
    write: async (id, offset, b64) => {
      await tick()
      const w = writes.get(id)
      if (offset !== w.bytes.length) throw new Error('bad offset')
      if (failWriteAt >= 0 && offset >= failWriteAt) throw new Error('disk full')
      w.bytes = Buffer.concat([w.bytes, Buffer.from(b64, 'base64')])
      return w.bytes.length
    },
    hash: async (path) => createHash('sha256').update(src[path]).digest('hex'),
    finish: async (id, sha) => {
      const w = writes.get(id)
      writes.delete(id)
      const got = createHash('sha256').update(corrupt ? Buffer.from('x') : w.bytes).digest('hex')
      if (got !== sha) throw new Error('integrity check failed: SHA-256 mismatch')
      dest[w.path] = w.bytes
      return w.path
    },
    abort: async (id) => {
      writes.delete(id)
      log.aborted.push(id)
    },
  }
  return { io, dest, log, writes }
}

const job = (src, size, name = 'out') => ({ src, destDir: '/d', name, size, conflict: 'fail' })

test('copyFile streams chunks in order and checks the hash', async () => {
  const data = Buffer.from('0123456789abcdefghij')
  const { io, dest } = fakeIo({ src: { '/s': data, '/empty': Buffer.alloc(0) } })
  const progress = []
  const path = await copyFile(job('/s', data.length), io, { canceled: () => false, chunk: 6, onProgress: (d) => progress.push(d) })
  assert.equal(path, '/d/out')
  assert.deepEqual(dest['/d/out'], data)
  assert.deepEqual(progress, [6, 12, 18, 20])
  assert.equal(await copyFile(job('/empty', 0, 'e'), io, { canceled: () => false }), '/d/e')
  assert.equal(base64Bytes(Buffer.from('ab').toString('base64')), 2)
  assert.equal(base64Bytes(''), 0)
})

test('copyFile aborts the write on failure, mismatch, a shrunk source and cancel', async () => {
  const data = Buffer.from('0123456789')
  let f = fakeIo({ src: { '/s': data }, failWriteAt: 4 })
  await assert.rejects(copyFile(job('/s', 10), f.io, { canceled: () => false, chunk: 4 }), /disk full/)
  assert.equal(f.log.aborted.length, 1)

  f = fakeIo({ src: { '/s': data }, corrupt: true })
  await assert.rejects(copyFile(job('/s', 10), f.io, { canceled: () => false }), /integrity/)

  f = fakeIo({ src: { '/s': data } })
  await assert.rejects(copyFile(job('/s', 15), f.io, { canceled: () => false, chunk: 4 }), /file_changed/)
  assert.equal(f.log.aborted.length, 1)

  f = fakeIo({ src: { '/s': data } })
  let reads = 0
  const canceled = () => reads > 1
  const counting = { ...f.io, read: (...a) => (reads++, f.io.read(...a)) }
  await assert.rejects(copyFile(job('/s', 10), counting, { canceled, chunk: 2 }), CanceledError)
  assert.equal(f.log.aborted.length, 1)
  assert.equal(f.writes.size, 0)
})

test('the queue runs two at a time, cancels, retries and reports progress', async () => {
  const src = {}
  for (const n of ['a', 'b', 'c', 'd']) src[`/${n}`] = Buffer.alloc(10, n)
  const up = fakeIo({ src, delay: 1, failWriteAt: -1 })
  const down = fakeIo({ src: { '/x': Buffer.from('xyz') }, delay: 1 })
  const settled = []
  const q = new TransferQueue({ io: { up: up.io, down: down.io }, chunk: 4, onSettled: (i) => settled.push([i.name, i.status]) })
  const mk = (direction, src, size, name) => ({ direction, src, destDir: '/d', name, size, conflict: 'fail', label: name })
  q.add([mk('up', '/a', 10, 'a'), mk('up', '/b', 10, 'b'), mk('up', '/c', 10, 'c'), mk('down', '/x', 3, 'x'), mk('up', '/d', 10, 'd')])
  assert.deepEqual(q.items.map((i) => i.status), ['running', 'running', 'queued', 'queued', 'queued'])
  q.cancel(q.items[4].id)
  assert.equal(q.items[4].status, 'canceled')
  q.cancel(q.items[1].id)
  await q.idle()
  assert.deepEqual(q.items.map((i) => i.status), ['done', 'canceled', 'done', 'done', 'canceled'])
  assert.ok(up.log.maxOpen <= 2)
  assert.deepEqual(down.dest['/d/x'], Buffer.from('xyz'))
  assert.equal(up.writes.size, 0, 'the cancelled write was dropped')

  const p = queueProgress(q.items)
  assert.deepEqual([p.done, p.total, p.percent, p.finished, p.failed], [23, 23, 100, 3, 0])

  q.retry(q.items[1].id)
  await q.idle()
  assert.equal(q.items[1].status, 'done')
  assert.deepEqual(up.dest['/d/b'], src['/b'])
  assert.ok(settled.some(([n, s]) => n === 'b' && s === 'canceled'))
  q.clearFinished()
  assert.equal(q.items.length, 0)
})

test('failed items can be retried together', async () => {
  let fail = true
  const f = fakeIo({ src: { '/a': Buffer.from('aaaa') } })
  const io = { ...f.io, write: (...a) => (fail ? Promise.reject(new Error('link down')) : f.io.write(...a)) }
  const q = new TransferQueue({ io: { up: io, down: io } })
  q.add([{ direction: 'up', src: '/a', destDir: '/d', name: 'a', size: 4, conflict: 'fail', label: 'a' }])
  await q.idle()
  assert.equal(q.items[0].status, 'failed')
  assert.equal(q.items[0].error, 'link down')
  assert.equal(queueProgress(q.items).failed, 1)
  fail = false
  q.retryFailed()
  await q.idle()
  assert.equal(q.items[0].status, 'done')
  assert.equal(q.items[0].dest, '/d/a')
})

test('progress math', () => {
  const it = (status, size, done) => ({ status, size, done })
  const p = queueProgress([it('running', 100, 50), it('queued', 100, 0), it('done', 50, 50), it('failed', 1000, 10), it('canceled', 5, 0)])
  assert.deepEqual([p.done, p.total, p.percent, p.active, p.queued, p.finished, p.failed], [100, 250, 40, 1, 1, 1, 1])
  assert.equal(queueProgress([]).percent, 100)
  assert.equal(itemPercent(it('running', 3, 1)), 33)
  assert.equal(itemPercent(it('done', 0, 0)), 100)
  assert.equal(itemPercent(it('queued', 0, 0)), 0)
})

test('the in-flight budget caps bytes across transfers and serves waiters in order', async () => {
  assert.ok(wireBytes(TRANSFER_CHUNK) * 2 * 2 <= MAX_IN_FLIGHT_BYTES, 'two files × (write + next read) fit')
  const b = new ByteBudget(10)
  await b.acquire(6)
  const order = []
  const w1 = b.acquire(6).then(() => order.push('a'))
  const w2 = b.acquire(1).then(() => order.push('b'))
  await new Promise((r) => setTimeout(r, 0))
  assert.deepEqual(order, [], 'a later small request does not jump the queue')
  b.release(6)
  await Promise.all([w1, w2])
  assert.deepEqual(order, ['a', 'b'])
  assert.equal(b.inFlight, 7)
  await assert.rejects(b.acquire(11), /in-flight cap/)
})

test('copies never hold more than the budget, and give it all back', async () => {
  const src = {}
  for (const n of ['a', 'b', 'c']) src[`/${n}`] = Buffer.alloc(40, n)
  const budget = new ByteBudget(wireBytes(4) * 2)
  const f = fakeIo({ src, delay: 1 })
  let peak = 0
  const io = {
    ...f.io,
    read: (...a) => ((peak = Math.max(peak, budget.inFlight)), f.io.read(...a)),
    write: (...a) => ((peak = Math.max(peak, budget.inFlight)), f.io.write(...a)),
  }
  const q = new TransferQueue({ io: { up: io, down: io }, chunk: 4, parallel: 3, budget })
  const mk = (n) => ({ direction: 'up', src: `/${n}`, destDir: '/d', name: n, size: 40, conflict: 'fail', label: n })
  q.add([mk('a'), mk('b'), mk('c')])
  await q.idle()
  assert.deepEqual(q.items.map((i) => i.status), ['done', 'done', 'done'])
  assert.ok(peak <= budget.max, `peak ${peak}`)
  assert.equal(budget.inFlight, 0)

  // A failed copy frees what it held, including a read still under way.
  const failing = { ...f.io, write: () => Promise.reject(new Error('link down')) }
  await assert.rejects(copyFile(job('/a', 40), failing, { canceled: () => false, chunk: 4, budget }), /link down/)
  assert.equal(budget.inFlight, 0)
})
