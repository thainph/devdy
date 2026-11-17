// Unit tests for lib/sanitizeSecrets.ts (credential masking of dashboard texts).
// Run: node --test src/lib/__tests__/sanitizeSecrets.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { MASK, sanitizeSecrets as s } from '../sanitizeSecrets.ts'

test('plain commands pass through (whitespace collapsed)', () => {
  assert.equal(s('pnpm   test  --filter report'), 'pnpm test --filter report')
  assert.equal(s('git push -u origin main'), 'git push -u origin main')
  assert.equal(s('cat src/a.ts'), 'cat src/a.ts')
})

test('env assignments and secret-named key=value', () => {
  assert.equal(s('GH_TOKEN=abc gh pr list'), `GH_TOKEN=${MASK} gh pr list`)
  assert.equal(s('run --password=hunter2'), `run --password=${MASK}`)
  assert.equal(s('x api_key=zzz'), `x api_key=${MASK}`)
  assert.equal(s('node a.js mode=fast'), 'node a.js mode=fast')
})

test('secret flags and auth headers mask the next word', () => {
  assert.equal(s('cli --token abc123 next'), `cli --token ${MASK} next`)
  assert.equal(s('curl -H "Authorization: Bearer abc.def"'), `curl -H "Authorization: Bearer ${MASK}`)
  assert.equal(s('curl -H "X-Api-Key: k1"'), `curl -H "X-Api-Key: ${MASK}`)
})

test('token-looking words and URL credentials', () => {
  assert.equal(s('echo ghp_0123456789abcdef'), `echo ${MASK}`)
  assert.equal(s('echo a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6'), `echo ${MASK}`)
  assert.equal(s('git clone https://user:pw@github.com/o/r.git'), 'git clone https://github.com/o/r.git')
  assert.equal(s('curl https://api.x.com/v1?key=secret#frag'), 'curl https://api.x.com/v1')
})

test('curl -u user:pass', () => {
  assert.equal(s('curl -u admin:s3cret https://x.io'), `curl -u admin:${MASK} https://x.io`)
  assert.equal(s('curl --user bob:pw x'), `curl --user bob:${MASK} x`)
})

test('-p passwords only in sshpass / mysql commands', () => {
  assert.equal(s('sshpass -p hunter2 ssh host'), `sshpass -p ${MASK} ssh host`)
  assert.equal(s('mysql -uroot -phunter2 db'), `mysql -uroot -p${MASK} db`)
  assert.equal(s('/usr/bin/mysqldump -p pw db'), `/usr/bin/mysqldump -p ${MASK} db`)
  assert.equal(s('mkdir -p src/lib'), 'mkdir -p src/lib')
})

test('JSON secret-named values', () => {
  assert.equal(s('{"user":"a","password":"x y"}'), `{"user":"a","password":"${MASK}"}`)
  assert.equal(s('{"accessToken": "abc", "n": 1}'), `{"accessToken": "${MASK}", "n": 1}`)
  assert.equal(s('{"client_secret":"q"}'), `{"client_secret":"${MASK}"}`)
})
