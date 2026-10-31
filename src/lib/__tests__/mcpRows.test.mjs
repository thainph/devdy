// Unit tests for lib/mcpRows.ts (MCP servers screen: rows, filters, form).
// Run: node --test src/lib/__tests__/mcpRows.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  GROUP_BUILTIN,
  GROUP_CATALOG,
  MCP_CATALOG,
  availableCatalog,
  blankForm,
  builtinRow,
  catalogRow,
  copyName,
  duplicateForm,
  formFromServer,
  formPayload,
  formSnapshot,
  groupMcpRows,
  mcpRowType,
  matchesMcpRow,
  parseArgs,
  serverLine,
  serverRow,
  toSecretEntries,
  validateForm,
  validateTarget,
} from '../mcpRows.ts'

const fs = {
  id: 'a1',
  name: 'filesystem',
  description: 'Local files',
  transport: 'stdio',
  command: 'npx',
  args: ['-y', '@modelcontextprotocol/server-filesystem'],
  url: null,
  env_keys: ['ROOT'],
  header_keys: [],
  enabled: true,
}
const remote = {
  id: 'b2',
  name: 'context',
  description: '',
  transport: 'http',
  command: null,
  args: [],
  url: 'https://mcp.example.com/mcp',
  env_keys: [],
  header_keys: ['Authorization'],
  enabled: false,
}

const labels = { builtin: 'Built-in', catalog: 'Recommended', transport: (t) => t.toUpperCase() }

test('serverLine joins command + args, or shows the URL', () => {
  assert.equal(serverLine(fs), 'npx -y @modelcontextprotocol/server-filesystem')
  assert.equal(serverLine(remote), 'https://mcp.example.com/mcp')
  assert.equal(serverLine({ transport: 'stdio', command: null, args: [], url: null }), '')
})

test('availableCatalog hides names already taken', () => {
  const left = availableCatalog(MCP_CATALOG, [{ name: 'context7' }])
  assert.equal(left.length, MCP_CATALOG.length - 1)
  assert.ok(!left.some((c) => c.name === 'context7'))
})

test('matchesMcpRow searches name/line/description', () => {
  const on = serverRow(fs)
  const off = serverRow(remote)
  assert.equal(matchesMcpRow(on, 'server-filesystem'), true)
  assert.equal(matchesMcpRow(on, 'LOCAL npx'), true)
  assert.equal(matchesMcpRow(off, 'example.com'), true)
  assert.equal(matchesMcpRow(on, 'sora'), false)
  assert.equal(matchesMcpRow(catalogRow(MCP_CATALOG[0]), ''), true)
})

test('builtin rows count as enabled only when active', () => {
  const active = builtinRow({ name: 'devdy', tools: 'mcp__devdy__*', tone: 'success' }, 'd')
  const setup = builtinRow({ name: 'google', tools: 'mcp__google__*', tone: 'warning' }, 'g')
  assert.equal(active.id, 'builtin:devdy')
  assert.equal(active.enabled, true)
  assert.equal(setup.enabled, false)
})

test('groupMcpRows: built-in first, transports in fixed order, catalog last', () => {
  const sse = { ...serverRow(remote), id: 'c3', transport: 'sse' }
  const rows = [
    catalogRow(MCP_CATALOG[1]),
    sse,
    serverRow(remote),
    serverRow(fs),
    builtinRow({ name: 'devdy', tools: 'mcp__devdy__*', tone: 'success' }, ''),
  ]
  assert.deepEqual(
    groupMcpRows(rows, labels).map((g) => [g.key, g.label, g.rows.map((r) => r.id)]),
    [
      [GROUP_BUILTIN, 'Built-in', ['builtin:devdy']],
      ['stdio', 'STDIO', ['a1']],
      ['http', 'HTTP', ['b2']],
      ['sse', 'SSE', ['c3']],
      [GROUP_CATALOG, 'Recommended', ['catalog:context7']],
    ],
  )
  assert.deepEqual(groupMcpRows([serverRow(fs)], labels).map((g) => g.key), ['stdio'])
})

test('formFromServer marks stored secrets as existing with no value', () => {
  const form = formFromServer(fs)
  assert.equal(form.argsText, '-y\n@modelcontextprotocol/server-filesystem')
  assert.deepEqual(form.envRows, [{ key: 'ROOT', value: '', existing: true }])
  assert.deepEqual(formFromServer(remote).headerRows, [{ key: 'Authorization', value: '', existing: true }])
})

test('toSecretEntries keeps stored values when left blank', () => {
  assert.deepEqual(
    toSecretEntries([
      { key: ' KEEP ', value: '', existing: true },
      { key: 'NEW', value: 's3cret', existing: false },
      { key: 'CHANGED', value: 'x', existing: true },
      { key: '  ', value: 'ignored', existing: false },
    ]),
    [{ key: 'KEEP' }, { key: 'NEW', value: 's3cret' }, { key: 'CHANGED', value: 'x' }],
  )
})

test('parseArgs drops blank lines and trims', () => {
  assert.deepEqual(parseArgs(' -y \n\n pkg \n'), ['-y', 'pkg'])
})

test('formPayload sends only the transport-relevant fields', () => {
  const stdio = formPayload({ ...formFromServer(fs), url: 'https://left.over' })
  assert.equal(stdio.url, null)
  assert.deepEqual(stdio.args, ['-y', '@modelcontextprotocol/server-filesystem'])
  assert.deepEqual(stdio.env, [{ key: 'ROOT' }])
  const http = formPayload({ ...formFromServer(remote), command: 'npx', argsText: 'x' })
  assert.equal(http.command, null)
  assert.deepEqual(http.args, [])
  assert.deepEqual(http.env, [])
  assert.deepEqual(http.headers, [{ key: 'Authorization' }])
})

test('validateForm reports the first problem as an i18n key', () => {
  const ok = formFromServer(fs)
  assert.equal(validateForm(ok), null)
  assert.deepEqual(validateForm({ ...ok, name: ' ' }), { key: 'nameRequired' })
  assert.deepEqual(validateForm({ ...ok, name: 'bad name' }), { key: 'nameFormat' })
  assert.deepEqual(validateForm({ ...ok, command: '' }), { key: 'commandRequired' })
  assert.deepEqual(validateForm({ ...formFromServer(remote), url: '' }), {
    key: 'urlRequired',
    params: { transport: 'http' },
  })
  assert.deepEqual(validateForm({ ...ok, envRows: [{ key: '', value: '', existing: false }] }), { key: 'keyEmpty' })
  const dup = [
    { key: 'A', value: '', existing: true },
    { key: 'A ', value: '1', existing: false },
  ]
  assert.deepEqual(validateForm({ ...ok, envRows: dup }), { key: 'duplicateKey', params: { key: 'A' } })
  // Header rows are ignored for stdio (and env rows for http).
  assert.equal(validateForm({ ...ok, headerRows: dup }), null)
  assert.equal(validateTarget({ transport: 'sse', command: '', url: 'https://x' }), null)
})

test('duplicateForm drops secret values and copyName avoids collisions', () => {
  const copy = duplicateForm(formFromServer(fs), 'filesystem-copy')
  assert.equal(copy.name, 'filesystem-copy')
  assert.deepEqual(copy.envRows, [{ key: 'ROOT', value: '', existing: false }])
  assert.equal(copyName('fs', ['fs']), 'fs-copy')
  assert.equal(copyName('fs', ['fs', 'fs-copy', 'fs-copy-2']), 'fs-copy-3')
})

test('formSnapshot tracks edits and optionally the enabled switch', () => {
  const a = formFromServer(fs)
  const b = { ...a, enabled: false }
  assert.equal(formSnapshot(a, false), formSnapshot(b, false))
  assert.notEqual(formSnapshot(a, true), formSnapshot(b, true))
  assert.notEqual(formSnapshot(blankForm(), true), formSnapshot({ ...blankForm(), name: 'x' }, true))
})

test('mcpRowType maps rows to their sidebar folder', () => {
  const builtin = { id: 'builtin:devdy', kind: 'builtin', transport: 'stdio' }
  const catalog = { id: 'catalog:x', kind: 'catalog', transport: 'http' }
  const server = { id: 's1', kind: 'server', transport: 'sse' }
  assert.deepEqual([builtin, catalog, server].map(mcpRowType), ['builtin', 'catalog', 'sse'])
})
