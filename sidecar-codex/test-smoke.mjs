// Standalone smoke test for the Codex sidecar, mimicking how the Rust broker
// drives it: spawn → send a prompt → auto-allow the permission request →
// collect translated stream-json → assert we saw a result and the edit landed.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { writeFileSync, readFileSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { execSync } from 'node:child_process'

const here = dirname(fileURLToPath(import.meta.url))
const work = mkdtempSync(join(tmpdir(), 'devdy-codex-'))
writeFileSync(join(work, 'README.md'), '# scratch\n')
try { execSync('git init -q && git add -A && git -c user.email=t@t.co -c user.name=t commit -qm init', { cwd: work }) } catch {}

const child = spawn('node', [join(here, 'index.mjs')], {
  cwd: work,
  stdio: ['pipe', 'pipe', 'pipe'],
  env: { ...process.env, DEVDY_PERMISSION_MODE: 'default' },
})

let sawSystem = false, sawAssistantText = false, sawToolUse = false, sawResult = false, sawPermission = false
let buf = ''
child.stdout.on('data', (c) => {
  buf += c.toString()
  let nl
  while ((nl = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, nl).trim(); buf = buf.slice(nl + 1)
    if (!line) continue
    let m; try { m = JSON.parse(line) } catch { continue }
    if (m.type === '_devdy_permission_request') {
      sawPermission = true
      console.log(`  ⟵ permission: ${m.tool_name} — ${JSON.stringify(m.tool_input).slice(0, 80)}`)
      child.stdin.write(JSON.stringify({ type: 'permission_response', requestId: m.requestId, decision: 'allow' }) + '\n')
    } else if (m.type === '_devdy_ready') {
      console.log(`  ✓ ready (thread ${m.threadId})`)
    } else if (m.type === '_devdy_error') {
      console.log(`  ✗ error: ${m.error}`)
    } else if (m.type === 'system') {
      sawSystem = true; console.log(`  ✓ system.init model=${m.model} session=${m.session_id}`)
    } else if (m.type === 'assistant') {
      for (const b of m.message?.content ?? []) {
        if (b.type === 'text') { sawAssistantText = true; process.stdout.write(`  ◇ text: ${b.text}\n`) }
        if (b.type === 'tool_use') { sawToolUse = true; console.log(`  ◆ tool_use ${b.name}: ${JSON.stringify(b.input).slice(0, 80)}`) }
      }
    } else if (m.type === 'result') {
      sawResult = true
    }
  }
})
child.stderr.on('data', (d) => process.stderr.write(`[stderr] ${d}`))
child.on('exit', () => {
  const content = readFileSync(join(work, 'README.md'), 'utf8')
  const edited = content.includes('devdy-codex-ok')
  console.log('\n──────── RESULT ────────')
  console.log({ sawSystem, sawAssistantText, sawToolUse, sawPermission, sawResult, fileEdited: edited })
  const pass = sawSystem && sawResult && sawPermission && edited
  console.log(`VERDICT: ${pass ? 'PASS ✅' : 'FAIL ❌'}`)
  process.exit(pass ? 0 : 1)
})

// Send the first prompt the way start_run does.
child.stdin.write(JSON.stringify({
  type: 'prompt',
  text: 'Append a line "devdy-codex-ok" to README.md (the sandbox may require escalated write approval). Then reply DONE.',
}) + '\n')

setTimeout(() => { console.log('⏱ timeout'); try { child.kill() } catch {} }, 150000)
