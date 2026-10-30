// Unit tests for lib/syncOnDemand.ts (on-demand sync: categories, excludes, formatting, preview).
// Run: node --test src/lib/__tests__/syncOnDemand.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  SYNC_CATEGORY_IDS,
  allCategoriesOn,
  backupDir,
  canApplyPreview,
  categoriesEnabled,
  defaultSelection,
  etaSeconds,
  excludesToText,
  folderPercent,
  folderProjects,
  formatBytes,
  fileCount,
  formatEta,
  isFolderFinal,
  isPlanChanged,
  isTruncated,
  isRemapped,
  localFolderState,
  overallPercent,
  parseExcludes,
  parseLastRun,
  parseMaxFileMb,
  parseTimestamp,
  pickSource,
  previewIsNoop,
  previewTotals,
  setCategory,
  summarizePreview,
} from '../syncOnDemand.ts'
import { syncErrorCode } from '../syncCore.ts'

test('setCategory keeps known ids in display order', () => {
  assert.deepEqual(setCategory([], 'slack', false), ['slack'])
  assert.deepEqual(setCategory(['slack'], 'config', false), ['config', 'slack'])
  assert.deepEqual(setCategory(['config', 'slack'], 'config', true), ['slack'])
  assert.deepEqual(setCategory(['bogus', 'notes'], 'notes', true), [])
  assert.deepEqual(setCategory(['notes'], 'notes', false), ['notes'])
})

test('allCategoriesOn / categoriesEnabled', () => {
  assert.equal(allCategoriesOn([]), true)
  assert.equal(allCategoriesOn(['unknown']), true)
  assert.equal(allCategoriesOn(['secrets']), false)
  assert.deepEqual(categoriesEnabled(['notes', 'slack'], ['slack']), ['notes'])
  assert.ok(SYNC_CATEGORY_IDS.includes('secrets'))
  assert.equal(SYNC_CATEGORY_IDS.length, 9)
})

test('parseExcludes trims, drops blanks / comments / duplicates', () => {
  assert.deepEqual(parseExcludes('  *.log\n\n# note\r\ntmp/\n*.log\n'), ['*.log', 'tmp/'])
  assert.deepEqual(parseExcludes(''), [])
  assert.equal(excludesToText(['a', 'b']), 'a\nb')
  assert.equal(excludesToText(null), '')
})

test('parseMaxFileMb accepts whole MB in range', () => {
  assert.equal(parseMaxFileMb('500'), 500)
  assert.equal(parseMaxFileMb(' 12 '), 12)
  assert.equal(parseMaxFileMb(1), 1)
  assert.equal(parseMaxFileMb('0'), null)
  assert.equal(parseMaxFileMb('1.5'), null)
  assert.equal(parseMaxFileMb('abc'), null)
  assert.equal(parseMaxFileMb(''), null)
  assert.equal(parseMaxFileMb('100001'), null)
})

test('localFolderState / isRemapped / folderProjects', () => {
  assert.equal(localFolderState({ local_path: '/a', exists: true }), 'ok')
  assert.equal(localFolderState({ local_path: '/a', exists: false }), 'create')
  assert.equal(localFolderState({ local_path: '', exists: false }), 'pick')
  assert.equal(localFolderState({ local_path: null, exists: false }), 'pick')
  assert.equal(isRemapped({ local_path: '/b', synced_path: '/a', mapped: true }), true)
  assert.equal(isRemapped({ local_path: '/a', synced_path: '/a', mapped: true }), false)
  assert.equal(isRemapped({ local_path: '/b', synced_path: '/a', mapped: false }), false)
  const ps = [
    { project_id: 'a', enabled: true, folder: true },
    { project_id: 'b', enabled: false, folder: true },
    { project_id: 'c', enabled: true, folder: false },
  ]
  assert.deepEqual(folderProjects(ps).map((p) => p.project_id), ['a'])
})

test('formatBytes uses binary units with one decimal under 10', () => {
  assert.equal(formatBytes(0), '0 B')
  assert.equal(formatBytes(null), '0 B')
  assert.equal(formatBytes(-5), '0 B')
  assert.equal(formatBytes(512), '512 B')
  assert.equal(formatBytes(1536), '1.5 KB')
  assert.equal(formatBytes(1024 * 1024), '1 MB')
  assert.equal(formatBytes(38 * 1024 * 1024), '38 MB')
  assert.equal(formatBytes(410.4 * 1024 * 1024), '410 MB')
  assert.equal(formatBytes(1.25 * 1024 ** 3), '1.3 GB')
})

test('formatEta', () => {
  assert.equal(formatEta(null), null)
  assert.equal(formatEta(-1), null)
  assert.equal(formatEta(Infinity), null)
  assert.equal(formatEta(0), '0s')
  assert.equal(formatEta(44.2), '45s')
  assert.equal(formatEta(120), '2m')
  assert.equal(formatEta(200), '3m 20s')
  assert.equal(formatEta(3600), '1h')
  assert.equal(formatEta(3900), '1h 5m')
})

test('folderPercent / overallPercent / isFolderFinal', () => {
  assert.equal(folderPercent(null), 0)
  assert.equal(folderPercent({ phase: 'files', done_bytes: 50, total_bytes: 200 }), 25)
  assert.equal(folderPercent({ phase: 'files', done_bytes: 500, total_bytes: 200 }), 100)
  assert.equal(folderPercent({ phase: 'git', done_bytes: 0, total_bytes: 0 }), 0)
  assert.equal(folderPercent({ phase: 'done', done_bytes: 0, total_bytes: 0 }), 100)
  assert.equal(overallPercent([]), 0)
  assert.equal(
    overallPercent([
      { phase: 'files', done_bytes: 100, total_bytes: 300 },
      { phase: 'done', done_bytes: 0, total_bytes: 100 },
    ]),
    50,
  )
  assert.equal(overallPercent([{ phase: 'done', done_bytes: 0, total_bytes: 0 }, { phase: 'git', done_bytes: 0, total_bytes: 0 }]), 50)
  assert.equal(isFolderFinal({ phase: 'skipped' }), true)
  assert.equal(isFolderFinal({ phase: 'files' }), false)
  assert.equal(isFolderFinal(undefined), false)
})

test('etaSeconds measures the average rate since the start', () => {
  assert.equal(etaSeconds(100, 1000, 0, 500), null)
  assert.equal(etaSeconds(0, 1000, 0, 5000), null)
  assert.equal(etaSeconds(200, 1000, 0, 2000), 8)
  assert.equal(etaSeconds(300, 1000, 0, 2000, 100), 7)
})

const preview = (over = {}) => ({
  project_id: 'p1',
  name: 'devdy',
  local_path: '/w/devdy',
  create: false,
  skipped: null,
  repos: [],
  overwrite: [],
  add: [],
  delete: [],
  local_changes: [],
  too_large: [],
  download_bytes: 0,
  ...over,
})
const repo = (over = {}) => ({
  path: '',
  clone: false,
  branch_from: 'main',
  branch_to: 'main',
  head: 'abcdef1234567',
  commits: 0,
  staged: 0,
  dirty: 0,
  bundle_bytes: 0,
  ...over,
})

test('summarizePreview counts branch moves, commits, files and backups', () => {
  const s = summarizePreview(preview({
    repos: [
      repo({ branch_to: 'feat/x', commits: 3, staged: 2 }),
      repo({ path: 'sub', commits: 0 }),
      repo({ path: 'new', clone: true }),
    ],
    overwrite: [{ path: 'a', size: 1 }],
    add: [{ path: 'b', size: 1 }, { path: 'c', size: 1 }],
    delete: [{ path: 'd', size: 1 }],
    local_changes: ['x', 'y'],
    download_bytes: 42,
  }))
  assert.deepEqual(s.branches, [{ repo: '', from: 'main', to: 'feat/x', sha: 'abcdef1' }])
  assert.equal(s.clones, 1)
  assert.equal(s.commits, 3)
  assert.equal(s.staged, 2)
  assert.deepEqual([s.overwrite, s.add, s.delete, s.bytes, s.localChanges], [1, 2, 1, 42, 2])
  assert.equal(previewIsNoop(s), false)
  assert.equal(previewIsNoop(summarizePreview(preview({ repos: [repo()] }))), true)
})

test('summarizePreview tolerates missing lists', () => {
  const s = summarizePreview({ project_id: 'p', name: 'n', download_bytes: 0 })
  assert.equal(s.overwrite, 0)
  assert.deepEqual(s.branches, [])
})

test('summarizePreview uses the exact counts, not the capped lists', () => {
  const capped = Array.from({ length: 3 }, (_, i) => ({ path: `f${i}`, size: 1 }))
  const s = summarizePreview(preview({
    overwrite: capped, overwrite_count: 12,
    add: capped, add_count: 3,
    delete: capped, delete_count: 50_000,
  }))
  assert.deepEqual([s.overwrite, s.add, s.delete], [12, 3, 50_000])
  assert.equal(s.truncated, true)
  assert.equal(summarizePreview(preview({ add: capped, add_count: 3 })).truncated, false)
  assert.equal(fileCount(capped, undefined), 3)
  assert.equal(fileCount(capped, 1), 3)
  assert.equal(fileCount(null, 7), 7)
  assert.equal(isTruncated(capped, 4), true)
  assert.equal(isTruncated(capped, 3), false)
  assert.equal(isTruncated(capped, null), false)
})

test('summarizePreview reports a same-branch rewind and skipped items', () => {
  const s = summarizePreview(preview({
    repos: [repo({ head: 'aaaaaaa1', head_from: 'bbbbbbb2', branches_rewound: ['main'] }), repo({ path: 'lib' })],
    skipped_items: [{ path: 'vendor/x', reason: 'submodule' }, { path: 'big.bin', reason: 'too_large', size: 9 }],
  }))
  assert.deepEqual(s.branches, [{ repo: '', from: 'main', to: 'main', sha: 'aaaaaaa' }])
  assert.deepEqual(s.rewound, [{ repo: '', branches: ['main'] }])
  assert.equal(s.skippedItems, 2)
  assert.equal(previewIsNoop(s), false)
  const same = summarizePreview(preview({ repos: [repo({ head: 'abc', head_from: 'abc' })] }))
  assert.equal(previewIsNoop(same), true)
  // A rewound branch other than the checked-out one still counts as a change.
  assert.equal(previewIsNoop(summarizePreview(preview({ repos: [repo({ branches_rewound: ['old'] })] }))), false)
})

test('previewTotals sums exact file counts and backs up rewound branches', () => {
  const list = [
    preview({ project_id: 'a', add: [{ path: 'x', size: 1 }], add_count: 4000, delete_count: 10 }),
    preview({ project_id: 'b', repos: [repo({ branches_rewound: ['main'] })] }),
  ]
  assert.deepEqual(previewTotals(list, ['a']), { count: 1, bytes: 0, backup: true, files: 4010 })
  assert.deepEqual(previewTotals(list, ['b']), { count: 1, bytes: 0, backup: true, files: 0 })
})

test('isPlanChanged spots the apply abort for a grown plan', () => {
  assert.equal(isPlanChanged('plan_changed'), true)
  assert.equal(isPlanChanged('folder apply failed: plan_changed: 3 more files'), true)
  assert.equal(isPlanChanged('preview_stale'), true)
  assert.equal(isPlanChanged('The plan grew beyond the confirmed preview'), true)
  assert.equal(isPlanChanged('peer_offline'), false)
  assert.equal(isPlanChanged(null), false)
})

test('previewTotals / defaultSelection skip skipped and failed projects', () => {
  const list = [
    preview({ project_id: 'a', download_bytes: 10, local_changes: ['x'] }),
    preview({ project_id: 'b', download_bytes: 20 }),
    preview({ project_id: 'c', skipped: 'running_session', download_bytes: 99 }),
    preview({ project_id: 'd', error: 'boom' }),
  ]
  assert.equal(canApplyPreview(list[2]), false)
  assert.deepEqual(defaultSelection(list), ['a', 'b'])
  assert.deepEqual(previewTotals(list, ['a', 'b', 'c']), { count: 2, bytes: 30, backup: true, files: 0 })
  assert.deepEqual(previewTotals(list, ['b']), { count: 1, bytes: 20, backup: false, files: 0 })
})

test('parseLastRun round-trips and rejects junk', () => {
  const r = { at: 5, source_device: 'd1', ok: true, error: null, applied: 3, projects_ok: 1, projects_total: 2, project_errors: [] }
  assert.deepEqual(parseLastRun(JSON.stringify(r)), r)
  assert.equal(parseLastRun(null), null)
  assert.equal(parseLastRun('{'), null)
  assert.equal(parseLastRun('{"at":"x"}'), null)
})

test('parseTimestamp / backupDir', () => {
  assert.equal(parseTimestamp(null), null)
  assert.equal(parseTimestamp(''), null)
  assert.equal(parseTimestamp('1700000000'), 1_700_000_000_000)
  assert.equal(parseTimestamp(1_700_000_000_000), 1_700_000_000_000)
  assert.equal(parseTimestamp('2026-10-30T00:00:00Z'), Date.UTC(2026, 9, 30))
  assert.equal(parseTimestamp('nope'), null)
  assert.equal(backupDir('/w/p/'), '/w/p/.devdy/sync-backup')
})

test('pickSource prefers the saved device, then an online one', () => {
  const devs = [{ id: 'me', is_self: true }, { id: 'a', is_self: false }, { id: 'b', is_self: false }]
  assert.equal(pickSource('b', devs, null), 'b')
  assert.equal(pickSource('me', devs, ['b']), 'b')
  assert.equal(pickSource('gone', devs, []), 'a')
  assert.equal(pickSource(null, [{ id: 'me', is_self: true }], null), null)
})

test('syncErrorCode knows peer_offline', () => {
  assert.equal(syncErrorCode('peer_offline'), 'peer_offline')
  assert.equal(syncErrorCode('sync run failed: peer_offline'), 'peer_offline')
})
