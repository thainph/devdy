// Runs as Tauri's beforeBundleCommand. The Claude sidecar is bundled as a plain
// resource directory, so a build from a fresh clone without
// `npm --prefix sidecar install` silently ships an app whose chat fails with
// "Cannot find package '@anthropic-ai/claude-agent-sdk'". Install the deps when
// missing and fail the bundle if the SDK or its native CLI is still absent.
import { execFileSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const sidecarDir = join(dirname(fileURLToPath(import.meta.url)), '..', 'sidecar')
const scope = join(sidecarDir, 'node_modules', '@anthropic-ai')
const sdk = join(scope, 'claude-agent-sdk', 'package.json')
const nativeCli = join(scope, `claude-agent-sdk-${process.platform}-${process.arch}`, 'claude')

const ready = () => existsSync(sdk) && existsSync(nativeCli)

if (!ready()) {
  console.log('[ensure-sidecar-deps] sidecar dependencies missing, running npm ci --omit=dev')
  execFileSync('npm', ['ci', '--omit=dev'], { cwd: sidecarDir, stdio: 'inherit' })
}

if (!ready()) {
  console.error(
    `[ensure-sidecar-deps] still missing ${existsSync(sdk) ? nativeCli : sdk}; ` +
      'run `npm --prefix sidecar install` and rebuild',
  )
  process.exit(1)
}
console.log('[ensure-sidecar-deps] sidecar dependencies OK')
