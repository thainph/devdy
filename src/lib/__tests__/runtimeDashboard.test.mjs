// Unit tests for lib/runtimeDashboard.ts (Runtime Dashboard: state, groups,
// folding, ordering, quota forecast, tool summaries, log lines).
// Run: node --test src/lib/__tests__/runtimeDashboard.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  QUIET_AFTER_MS,
  activityBuckets,
  buildTree,
  classifyState,
  compareSessions,
  contextPercent,
  displayOrder,
  filterCounts,
  formatElapsed,
  isMissingCommandError,
  isWaitTool,
  lastEvent,
  layoutTree,
  logLine,
  mapOrder,
  matchesFilter,
  matchesQuery,
  needsAttention,
  noteActivity,
  parentOf,
  pushQuotaSample,
  pushSample,
  quotaForecast,
  runningTool,
  sessionKind,
  stepOrder,
  stepQueue,
  toolLabel,
  toolSummary,
  waitQueue,
} from '../runtimeDashboard.ts'

const NOW = Date.parse('2026-11-17T10:00:00Z')

function s(id, over = {}) {
  return {
    id,
    runId: id,
    projectId: 'p1',
    projectName: 'Devdy',
    title: `Session ${id}`,
    engine: 'claude',
    role: 'session',
    conductorId: null,
    live: true,
    state: 'running',
    waitingSince: null,
    startedAt: NOW - 60_000,
    now: { kind: 'none', tool: null, detail: '', since: null },
    ctxPercent: 10,
    spark: [],
    cpuPercent: null,
    rssKb: null,
    ...over,
  }
}

const base = { pending: 0, conductor: false, currentTool: null, lastEventAt: NOW, attention: [], now: NOW }

test('classifyState: waiting beats everything but external', () => {
  assert.equal(classifyState({ ...base, pending: 1, attention: ['quiet'] }), 'waiting')
  assert.equal(classifyState({ ...base, external: true, pending: 1 }), 'external')
})

test('classifyState: a conductor between turns is idle, even when quiet', () => {
  const lastEventAt = NOW - QUIET_AFTER_MS - 1
  assert.equal(classifyState({ ...base, conductor: true, ended: true, lastEventAt, attention: ['quiet'] }), 'idle')
  assert.equal(classifyState({ ...base, conductor: true, ended: true, pending: 1 }), 'waiting')
  assert.equal(classifyState({ ...base, conductor: false, ended: true, lastEventAt }), 'quiet')
})

test('classifyState: a conductor in session_wait is idle, not quiet', () => {
  const lastEventAt = NOW - QUIET_AFTER_MS - 1
  assert.equal(classifyState({ ...base, conductor: true, currentTool: 'mcp__devdy-conductor__session_wait', lastEventAt }), 'idle')
  assert.equal(classifyState({ ...base, conductor: false, currentTool: 'mcp__x__session_wait', lastEventAt }), 'quiet')
})

test('classifyState: quiet from the watchdog or from silence', () => {
  assert.equal(classifyState({ ...base, attention: ['long_tool'] }), 'quiet')
  assert.equal(classifyState({ ...base, attention: ['permission_waiting'] }), 'running')
  assert.equal(classifyState({ ...base, lastEventAt: NOW - QUIET_AFTER_MS - 1 }), 'quiet')
  assert.equal(classifyState({ ...base, lastEventAt: NOW - QUIET_AFTER_MS + 1000 }), 'running')
  assert.equal(classifyState({ ...base, lastEventAt: null }), 'running')
})

test('isWaitTool', () => {
  assert.ok(isWaitTool('session_wait'))
  assert.ok(isWaitTool('mcp__conductor__session_wait'))
  assert.ok(!isWaitTool('mcp__conductor__session_waiting'))
  assert.ok(!isWaitTool(null))
})

test('contextPercent and needsAttention', () => {
  assert.equal(contextPercent(160_000, 200_000), 80)
  assert.equal(contextPercent(300_000, 200_000), 100)
  assert.equal(contextPercent(0, 200_000), 0)
  assert.equal(contextPercent(null, 200_000), null)
  assert.equal(contextPercent(10, null), null)
  assert.ok(needsAttention(s('a', { ctxPercent: 80 })))
  assert.ok(!needsAttention(s('a', { ctxPercent: 79 })))
  assert.ok(needsAttention(s('a', { state: 'quiet' })))
  assert.ok(!needsAttention(s('a', { state: 'idle' })))
})

test('compareSessions: waiting (longest first) → quiet → context full → running → idle → external', () => {
  const list = [
    s('ext', { state: 'external' }),
    s('idle', { state: 'idle' }),
    s('run', {}),
    s('ctx', { ctxPercent: 90 }),
    s('quiet', { state: 'quiet' }),
    s('w2', { state: 'waiting', waitingSince: NOW - 1000 }),
    s('w1', { state: 'waiting', waitingSince: NOW - 5000 }),
  ]
  assert.deepEqual(list.sort(compareSessions).map((x) => x.id), ['w1', 'w2', 'quiet', 'ctx', 'run', 'idle', 'ext'])
})

test('matchesQuery covers title and project', () => {
  assert.ok(matchesQuery(s('a', { title: 'Fix filter' }), 'FIX'))
  assert.ok(matchesQuery(s('a', { projectName: 'Lunchbox' }), 'lunch'))
  assert.ok(!matchesQuery(s('a'), 'nope'))
  assert.ok(matchesQuery(s('a'), '  '))
})

test('filterCounts', () => {
  const c = filterCounts([
    s('a', { state: 'waiting' }),
    s('b', { state: 'quiet', ctxPercent: 85 }),
    s('c'),
    s('d', { state: 'idle' }),
    s('e', { state: 'external' }),
  ])
  assert.deepEqual(c, { wait: 1, quiet: 1, ctx: 1, run: 1, idle: 1, ext: 1 })
})

test('matchesFilter by state', () => {
  assert.ok(matchesFilter(s('a'), 'run'))
  assert.ok(!matchesFilter(s('a'), 'idle'))
  assert.ok(matchesFilter(s('a', { state: 'idle' }), 'idle'))
  assert.ok(matchesFilter(s('a', { state: 'external' }), 'ext'))
  assert.ok(matchesFilter(s('a', { state: 'waiting' }), 'wait'))
  assert.ok(matchesFilter(s('a'), null))
})

function sample() {
  return [
    s('c1', { role: 'conductor', state: 'idle', startedAt: NOW - 10_000 }),
    s('c2', { role: 'conductor', state: 'idle', startedAt: NOW - 20_000 }),
    s('w1', { role: 'worker', conductorId: 'c1', state: 'waiting', waitingSince: NOW - 3000 }),
    s('w2', { role: 'worker', conductorId: 'c1' }),
    s('w3', { role: 'worker', conductorId: 'c2' }),
    s('orphan', { role: 'worker', conductorId: 'gone' }),
    s('solo1', {}),
    s('e1', { role: 'external', state: 'external', runId: null }),
  ]
}

test('buildTree: workers under their parent, stray workers top-level, urgent first, external last', () => {
  const nodes = buildTree(sample())
  const ids = nodes.map((n) => n.session.id)
  assert.equal(ids[0], 'c1') // its worker w1 waits for you
  assert.equal(ids.at(-1), 'e1')
  assert.deepEqual([...ids].sort(), ['c1', 'c2', 'e1', 'orphan', 'solo1'])
  const by = Object.fromEntries(nodes.map((n) => [n.session.id, n]))
  assert.deepEqual(by.c1.children.map((c) => c.id), ['w1', 'w2'])
  assert.deepEqual(by.c2.children.map((c) => c.id), ['w3'])
  assert.deepEqual(by.orphan.children, [])
})

const noLayout = { query: '', filter: null, open: {} }

test('layoutTree: workers show under their parent; few workers or one needing you open by default', () => {
  const rows = layoutTree(buildTree(sample()), noLayout)
  const ids = rows.map((r) => r.session.id)
  assert.deepEqual(ids.slice(0, 3), ['c1', 'w1', 'w2'])
  const c1 = rows[0]
  assert.equal(c1.depth, 0)
  assert.equal(c1.childCount, 2)
  assert.equal(c1.open, true)
  assert.equal(c1.childAttention, 1)
  assert.equal(rows[1].depth, 1)
  assert.equal(rows[1].last, false)
  assert.equal(rows[2].last, true)
})

test('layoutTree: many calm workers start collapsed; hand choices win', () => {
  const list = [s('p', { role: 'conductor' }), ...['a', 'b', 'c', 'd', 'e'].map((id) => s(id, { role: 'worker', conductorId: 'p' }))]
  const nodes = buildTree(list)
  const closed = layoutTree(nodes, noLayout)
  assert.deepEqual(closed.map((r) => r.session.id), ['p'])
  assert.equal(closed[0].open, false)
  assert.equal(closed[0].childCount, 5)
  const opened = layoutTree(nodes, { ...noLayout, open: { p: true } })
  assert.equal(opened.length, 6)
  const shut = layoutTree(buildTree(sample()), { ...noLayout, open: { c1: false } })
  assert.ok(!shut.some((r) => r.session.id === 'w1'))
})

test('layoutTree: search shows matching workers under their parent', () => {
  const list = sample()
  list.find((x) => x.id === 'w3').title = 'migrate invoice'
  const rows = layoutTree(buildTree(list), { ...noLayout, query: 'invoice' })
  assert.deepEqual(rows.map((r) => r.session.id), ['c2', 'w3'])
  assert.equal(rows[0].open, true)
  assert.equal(layoutTree(buildTree(list), { ...noLayout, query: 'zzz' }).length, 0)
})

test('layoutTree: matching a parent shows all its workers', () => {
  const list = sample()
  list.find((x) => x.id === 'c2').title = 'Migrate billing'
  const rows = layoutTree(buildTree(list), { ...noLayout, query: 'billing' })
  assert.deepEqual(rows.map((r) => r.session.id), ['c2', 'w3'])
})

test('layoutTree: filter chips keep the parent as context', () => {
  const rows = layoutTree(buildTree(sample()), { ...noLayout, filter: 'wait' })
  assert.deepEqual(rows.map((r) => r.session.id), ['c1', 'w1'])
})

test('displayOrder follows the rows; mapOrder lists everything; parentOf', () => {
  const nodes = buildTree(sample())
  const rows = layoutTree(nodes, noLayout)
  assert.deepEqual(displayOrder(rows), rows.map((r) => r.session.id))
  const all = mapOrder(nodes).map((x) => x.id)
  assert.equal(all.length, 8)
  assert.ok(all.indexOf('w3') === all.indexOf('c2') + 1)
  assert.equal(parentOf(nodes, 'w2'), 'c1')
  assert.equal(parentOf(nodes, 'c1'), null)
  assert.equal(parentOf(nodes, 'orphan'), null)
})

test('stepOrder clamps; stepQueue wraps', () => {
  assert.equal(stepOrder(['a', 'b'], 'b', 1), 'b')
  assert.equal(stepOrder(['a', 'b'], 'a', -1), 'a')
  assert.equal(stepOrder(['a', 'b'], null, 1), 'a')
  assert.equal(stepOrder([], null, 1), null)
  assert.equal(stepQueue(['a', 'b'], 'b', 1), 'a')
  assert.equal(stepQueue(['a', 'b'], 'a', -1), 'b')
  assert.equal(stepQueue(['a', 'b'], 'x', -1), 'b')
})

test('waitQueue: longest waiting first', () => {
  const list = [
    s('a', { state: 'waiting', waitingSince: NOW - 100 }),
    s('b', { state: 'waiting', waitingSince: NOW - 900 }),
    s('c', {}),
    s('d', { state: 'waiting', waitingSince: NOW - 500 }),
  ]
  const q = waitQueue(list)
  assert.deepEqual(q, ['b', 'd', 'a'])
})

test('activity fallback buckets entry growth per minute', () => {
  const tracks = new Map()
  noteActivity(tracks, 'r', 10, NOW)
  assert.equal(tracks.get('r').lastEventAt, null)
  noteActivity(tracks, 'r', 13, NOW + 1000)
  noteActivity(tracks, 'r', 13, NOW + 2000)
  noteActivity(tracks, 'r', 15, NOW + 61_000)
  const t = tracks.get('r')
  assert.equal(t.lastEventAt, NOW + 61_000)
  const b = activityBuckets(t, NOW + 61_000, 3)
  assert.deepEqual(b, [0, 3, 2])
  // entries cleared (a new turn reset): the new count is all new events
  noteActivity(tracks, 'r', 4, NOW + 62_000)
  assert.deepEqual(activityBuckets(t, NOW + 62_000, 1), [6])
  assert.deepEqual(activityBuckets(undefined, NOW, 2), [0, 0])
})

test('formatElapsed', () => {
  assert.equal(formatElapsed(42_000), '0:42')
  assert.equal(formatElapsed(12 * 60_000 + 5000), '12:05')
  assert.equal(formatElapsed(3600_000 + 12 * 60_000 + 40_000), '1:12:40')
  assert.equal(formatElapsed(null), '')
  assert.equal(formatElapsed(-5), '0:00')
})

test('quotaForecast: from the recent pace', () => {
  const resetsAt = NOW + 3 * 3600_000
  const samples = [{ at: NOW - 30 * 60_000, pct: 60 }]
  // 10 points per 30 min → 30 points left → 90 min
  const f = quotaForecast(70, resetsAt, samples, NOW)
  assert.equal(f.kind, 'exhaust')
  assert.equal(f.at, NOW + 90 * 60_000)
})

test('quotaForecast: resets first → safe; full; unknown', () => {
  const resetsAt = NOW + 30 * 60_000
  assert.deepEqual(quotaForecast(70, resetsAt, [{ at: NOW - 30 * 60_000, pct: 60 }], NOW), { kind: 'safe' })
  assert.deepEqual(quotaForecast(100, resetsAt, [], NOW), { kind: 'full' })
  assert.deepEqual(quotaForecast(null, resetsAt, [], NOW), { kind: 'unknown' })
  // flat pace
  assert.deepEqual(quotaForecast(40, NOW + 3600_000, [{ at: NOW - 20 * 60_000, pct: 40 }], NOW), { kind: 'safe' })
})

test('quotaForecast: falls back to the average since the window opened', () => {
  // window opened 2 h ago (reset in 3 h) at 40% → 20%/h → 60 points left → 3 h → exactly at reset → safe
  assert.deepEqual(quotaForecast(40, NOW + 3 * 3600_000, [], NOW), { kind: 'safe' })
  // 80% after 2 h → 40%/h → 30 min left
  const f = quotaForecast(80, NOW + 3 * 3600_000, [], NOW)
  assert.equal(f.kind, 'exhaust')
  assert.equal(f.at, NOW + 30 * 60_000)
  // window just opened: not enough to tell
  assert.deepEqual(quotaForecast(5, NOW + 5 * 3600_000 - 60_000, [], NOW), { kind: 'unknown' })
})

test('pushQuotaSample dedups within a minute and drops old samples', () => {
  let l = pushQuotaSample([], { at: NOW, pct: 10 })
  l = pushQuotaSample(l, { at: NOW + 1000, pct: 10 })
  assert.equal(l.length, 1)
  l = pushQuotaSample(l, { at: NOW + 2000, pct: 11 })
  assert.equal(l.length, 2)
  l = pushQuotaSample(l, { at: NOW + 7 * 3600_000, pct: 1 })
  assert.deepEqual(l, [{ at: NOW + 7 * 3600_000, pct: 1 }])
})

test('toolLabel / toolSummary across engines', () => {
  assert.equal(toolLabel('mcp__devdy__todos_list'), 'devdy todos_list')
  assert.equal(toolLabel('mcp__claude_ai_Gmail__search'), 'claude_ai_Gmail search')
  assert.equal(toolLabel('Bash'), 'Bash')
  assert.equal(toolSummary('Bash', { command: 'pnpm   test\n --filter x' }), 'pnpm test --filter x')
  assert.equal(toolSummary('Bash', { command: ['cargo', 'check'] }), 'cargo check')
  assert.equal(toolSummary('Edit', { file_path: '/Users/me/repo/src-tauri/src/runs/registry.rs' }), 'runs/registry.rs')
  // Codex fileChange / apply_patch
  assert.equal(toolSummary('Edit', { changes: [{ path: 'src/a.ts' }] }), 'src/a.ts')
  assert.equal(toolSummary('Edit', { changes: { 'lib/b.ts': {} } }), 'lib/b.ts')
  assert.equal(toolSummary('Edit', { input: '*** Begin Patch\n*** Update File: x/y/z.rs\n' }), 'y/z.rs')
  assert.equal(toolSummary('Grep', { pattern: 'invoiceId' }), 'invoiceId')
  assert.equal(toolSummary('AskUserQuestion', { questions: [{ question: 'Which one?' }] }), 'Which one?')
  assert.equal(toolSummary('X', null), '')
  assert.equal(toolSummary('Bash', { command: 'x'.repeat(300) }).length, 120)
})

test('logLine maps entries to compact kinds', () => {
  assert.deepEqual(logLine({ kind: 'tool', name: 'Bash', input: { command: 'ls' } }), { kind: 'tool', text: 'Bash ls' })
  assert.deepEqual(
    logLine({ kind: 'tool', name: 'Bash', input: { command: 'ls' }, result: { is_error: true } }),
    { kind: 'err', text: 'Bash ls' },
  )
  assert.deepEqual(logLine({ kind: 'tool', name: 'AskUserQuestion', input: { questions: [{ question: 'Q?' }] } }), { kind: 'ask', text: 'Q?' })
  assert.deepEqual(logLine({ kind: 'text', text: '\n\nHello\nworld' }), { kind: 'text', text: 'Hello' })
  assert.equal(logLine({ kind: 'text', text: '  ' }), null)
  assert.equal(logLine({ kind: 'thinking', text: 'hmm' }), null)
  assert.deepEqual(logLine({ kind: 'result', text: 'done', is_error: false }), { kind: 'ok', text: '' })
  assert.deepEqual(logLine({ kind: 'result', text: 'boom', is_error: true }), { kind: 'err', text: 'boom' })
  assert.equal(logLine({ kind: 'log', level: 'debug', text: 'x' }), null)
  assert.deepEqual(logLine({ kind: 'command', name: 'compact' }), { kind: 'note', text: '/compact' })
})

test('runningTool: the last tool without a result in the current turn', () => {
  assert.deepEqual(runningTool([{ kind: 'text', text: 'x' }, { kind: 'tool', name: 'Bash', input: {} }]), { name: 'Bash', input: {} })
  assert.equal(runningTool([{ kind: 'tool', name: 'Bash', input: {}, result: { is_error: false } }]), null)
  assert.equal(runningTool([{ kind: 'tool', name: 'Bash', input: {} }, { kind: 'result', text: '', is_error: false }]), null)
  assert.equal(runningTool([]), null)
})

test('lastEvent: the latest line with text, a tool keeps its name', () => {
  const entries = [
    { kind: 'tool', name: 'Bash', input: { command: 'cargo check' }, result: { is_error: false } },
    { kind: 'thinking', text: 'hmm' },
  ]
  assert.deepEqual(lastEvent(entries), { tool: 'Bash', text: 'Bash cargo check' })
  assert.deepEqual(lastEvent([...entries, { kind: 'text', text: 'All good' }]), { tool: null, text: 'All good' })
  // a finished turn has no text of its own: look past it
  assert.deepEqual(lastEvent([...entries, { kind: 'result', text: '', is_error: false }]), { tool: 'Bash', text: 'Bash cargo check' })
  assert.equal(lastEvent([]), null)
  assert.equal(lastEvent([{ kind: 'text', text: 'old' }, { kind: 'thinking', text: '' }], 1), null)
})

test('a conductor with live workers and no running tool waits on them (Codex)', () => {
  const lastEventAt = NOW - QUIET_AFTER_MS - 1
  assert.equal(classifyState({ ...base, conductor: true, liveWorkers: 2, currentTool: null, lastEventAt }), 'idle')
  assert.equal(classifyState({ ...base, conductor: true, liveWorkers: 2, currentTool: 'Bash' }), 'running')
  assert.equal(classifyState({ ...base, conductor: true, liveWorkers: 0, currentTool: null, lastEventAt }), 'quiet')
  assert.equal(classifyState({ ...base, conductor: true, liveWorkers: 2, pending: 1 }), 'waiting')
})

test('toolSummary masks credentials shown on cards and in the log', () => {
  assert.equal(toolSummary('Bash', { command: 'GH_TOKEN=abc gh pr list' }), 'GH_TOKEN=*** gh pr list')
  assert.equal(toolSummary('WebFetch', { url: 'https://x.io/a?token=1' }), 'https://x.io/a')
  assert.deepEqual(logLine({ kind: 'tool', name: 'Bash', input: { command: 'sshpass -p pw ssh h' } }), { kind: 'tool', text: 'Bash sshpass -p *** ssh h' })
})

test('isMissingCommandError tells a missing command from a transient failure', () => {
  assert.equal(isMissingCommandError('command get_runtime_snapshot not found'), true)
  assert.equal(isMissingCommandError(new Error('Unknown command get_runtime_snapshot')), true)
  assert.equal(isMissingCommandError('database is locked'), false)
  assert.equal(isMissingCommandError('timeout'), false)
})

test('sessionKind: issue / PR by run type, conductor, external, else session', () => {
  assert.equal(sessionKind({ role: 'session', runType: 'review_pr' }), 'pr')
  assert.equal(sessionKind({ role: 'worker', runType: 'analyze_issue' }), 'issue')
  assert.equal(sessionKind({ role: 'conductor', runType: 'session' }), 'conductor')
  assert.equal(sessionKind({ role: 'external', runType: null }), 'external')
  assert.equal(sessionKind({ role: 'worker', runType: 'session' }), 'session')
})
