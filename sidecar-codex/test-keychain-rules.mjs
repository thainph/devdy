// Every Codex session installs Devdy's execpolicy rule that forbids reading the
// macOS Keychain through `security` ($CODEX_HOME/rules/devdy-keychain.rules),
// and puts it back when it was changed. Uses the fixture app-server (no CLI).
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import assert from 'node:assert/strict'

const here = dirname(fileURLToPath(import.meta.url))
const codexHome = mkdtempSync(join(tmpdir(), 'devdy-codex-home-'))
const rules = join(codexHome, 'rules', 'devdy-keychain.rules')

function runSession() {
  const env = { ...process.env, CODEX_HOME: codexHome, DEVDY_CODEX_PATH: join(here, 'test-fixture-app-server.mjs') }
  delete env.DEVDY_RESUME_SESSION
  return new Promise((resolve) => {
    const child = spawn('node', [join(here, 'index.mjs')], {
      cwd: mkdtempSync(join(tmpdir(), 'devdy-codex-rules-')),
      stdio: ['pipe', 'pipe', 'pipe'],
      env,
    })
    const timer = setTimeout(() => child.kill('SIGKILL'), 10_000)
    let buf = ''
    child.stdout.on('data', (chunk) => {
      buf += chunk.toString()
      // Ready means the rule was handled (it is written before codex starts).
      if (buf.includes('"_devdy_ready"')) child.stdin.end()
    })
    child.on('exit', () => { clearTimeout(timer); resolve() })
  })
}

await runSession()
const first = readFileSync(rules, 'utf8')
assert.match(first, /prefix_rule\(/)
assert.match(first, /"find-generic-password"/)
assert.match(first, /decision = "forbidden"/)

writeFileSync(rules, '# tampered\n')
await runSession()
assert.equal(readFileSync(rules, 'utf8'), first)

console.log('ok   codex sessions install the Keychain execpolicy rule')
