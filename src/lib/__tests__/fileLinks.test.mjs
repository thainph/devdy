// Unit tests for lib/fileLinks.ts: the indexed `matchProjectFile` must resolve
// exactly like the original linear endsWith scan it replaced.
// Run: node --test src/lib/__tests__/fileLinks.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'

const { matchProjectFile, parseLineRef } = await import('../fileLinks.ts')

// The pre-index implementation, kept verbatim as the reference semantics.
function linearMatch(raw, fileSet) {
  if (!raw || raw.length > 200 || /\s/.test(raw)) return null
  let token = raw.trim().replace(/[)\].,:;'"`]+$/, '')
  if (token.startsWith('./')) token = token.slice(2)
  if (!token) return null
  if (fileSet.has(token)) return token
  if (token.includes('/')) {
    for (const p of fileSet) if (p === token || p.endsWith('/' + token)) return p
  } else if (/\.[a-zA-Z0-9]+$/.test(token)) {
    let hit = null
    for (const p of fileSet) {
      if (p === token || p.endsWith('/' + token)) {
        if (hit) return null
        hit = p
      }
    }
    if (hit) return hit
  }
  return null
}

const FILES = [
  'README.md',
  'package.json',
  'src/main.ts',
  'src/lib/fileLinks.ts',
  'src/lib/diff.ts',
  'src/components/DiffView.vue',
  'src/components/StreamLog.vue',
  'src/views/RunView.vue',
  'src-tauri/src/main.rs',
  'src-tauri/src/lib.rs',
  'sidecar/package.json',
  'sidecar-mcp/index.mjs',
  'docs/a.md',
  'docs/nested/a.md',
  'Makefile',
]

test('exact, suffix, bare-filename and ambiguity cases', () => {
  const set = new Set(FILES)
  assert.equal(matchProjectFile('src/lib/diff.ts', set), 'src/lib/diff.ts')
  assert.equal(matchProjectFile('./src/main.ts', set), 'src/main.ts')
  assert.equal(matchProjectFile('lib/diff.ts', set), 'src/lib/diff.ts')
  assert.equal(matchProjectFile('StreamLog.vue', set), 'src/components/StreamLog.vue')
  assert.equal(matchProjectFile('StreamLog.vue),', set), 'src/components/StreamLog.vue')
  // Two files named main.* in different dirs: bare `main.rs` is unique, but
  // `package.json` exists at the root so the exact hit wins.
  assert.equal(matchProjectFile('main.rs', set), 'src-tauri/src/main.rs')
  assert.equal(matchProjectFile('package.json', set), 'package.json')
  // Ambiguous bare name → null; a disambiguating dir → first match.
  assert.equal(matchProjectFile('a.md', set), null)
  assert.equal(matchProjectFile('nested/a.md', set), 'docs/nested/a.md')
  // `src/main.ts` vs `src-tauri/src/main.rs`: the suffix must align on a slash.
  assert.equal(matchProjectFile('src/main.rs', set), 'src-tauri/src/main.rs')
  // Rejects: whitespace, too long, no extension bare word, trailing slash.
  assert.equal(matchProjectFile('foo bar.ts', set), null)
  assert.equal(matchProjectFile('x'.repeat(201), set), null)
  assert.equal(matchProjectFile('Makefile', set), 'Makefile')
  assert.equal(matchProjectFile('main', set), null)
  assert.equal(matchProjectFile('src/', set), null)
  assert.equal(matchProjectFile('', set), null)
})

test('agrees with the linear scan on every token shape', () => {
  const set = new Set(FILES)
  const tokens = new Set()
  for (const f of FILES) {
    const parts = f.split('/')
    for (let k = 0; k < parts.length; k++) tokens.add(parts.slice(k).join('/'))
    tokens.add(`./${f}`)
    tokens.add(`${f}.`)
    tokens.add(`\`${parts[parts.length - 1]}\``)
  }
  for (const t of ['nope.ts', 'a/b/', '/', './', 'diff', 'lib/', '/src/main.ts', 'tauri/src/lib.rs']) tokens.add(t)
  for (const t of tokens) assert.equal(matchProjectFile(t, set), linearMatch(t, set), `token ${t}`)
})

test('a new Set is re-indexed; the memo does not leak across sets', () => {
  const a = new Set(['src/x.ts'])
  assert.equal(matchProjectFile('x.ts', a), 'src/x.ts')
  const b = new Set(['src/x.ts', 'lib/x.ts'])
  assert.equal(matchProjectFile('x.ts', b), null)
  assert.equal(matchProjectFile('x.ts', a), 'src/x.ts')
})

test('scales to a large project', () => {
  const files = []
  for (let i = 0; i < 50_000; i++) files.push(`pkg${i % 500}/dir${i % 37}/file${i}.ts`)
  const set = new Set(files)
  const t0 = performance.now()
  for (let i = 0; i < 2000; i++) matchProjectFile(`file${i * 7}.ts`, set)
  assert.ok(performance.now() - t0 < 1000)
  assert.equal(matchProjectFile('file123.ts', set), 'pkg123/dir12/file123.ts')
})

test('parseLineRef', () => {
  assert.equal(parseLineRef('a.ts#L42'), 42)
  assert.equal(parseLineRef('a.ts:7-9'), 7)
  assert.equal(parseLineRef('a.ts'), null)
})
