// Terraform config scanner for the Devdy terraform shim.
//
// Before a terraform call is sent to the broker, the shim scans the config for
// code that runs OUTSIDE Terraform — `data "external"`, `provisioner
// "local-exec"`, `data "http"` — because during `plan`/`validate`/`console` it
// would execute with the linked cloud credentials. The broker turns an
// otherwise-allowed evaluating command into an approval prompt when found.
//
// Scope: `*.tf` / `*.tf.json` in the working dir, recursively in LOCAL module
// sources (`source = "./..."` / `"../..."`), and everything under
// `.terraform/modules` (downloaded remote modules). Fail-closed: any read error
// or hitting a scan limit counts as detected.

import fs from 'node:fs'
import path from 'node:path'

const MAX_DEPTH = 12
const MAX_FILES = 2000
const MAX_HITS = 5

const HCL_PATTERNS = [
  [/\bdata\s+"external"/, 'data.external'],
  [/"local-exec"/, 'local-exec'],
  [/\bdata\s+"http"/, 'data.http'],
]

// .tf.json: a simple substring heuristic is enough (keys under "data" /
// "provisioner").
function jsonKinds(text) {
  const kinds = []
  const hasData = /"data"\s*:/.test(text)
  if (hasData && /"external"\s*:/.test(text)) kinds.push('data.external')
  if (/"local-exec"/.test(text)) kinds.push('local-exec')
  if (hasData && /"http"\s*:/.test(text)) kinds.push('data.http')
  return kinds
}

function hclKinds(text) {
  return HCL_PATTERNS.filter(([re]) => re.test(text)).map(([, kind]) => kind)
}

// Local module sources: only module blocks use `./` / `../` paths (provider
// sources look like `hashicorp/aws`), so a global match is sufficient.
function localSources(text, isJson) {
  const re = isJson
    ? /"source"\s*:\s*"(\.\.?\/[^"]*)"/g
    : /\bsource\s*=\s*"(\.\.?\/[^"]*)"/g
  const out = []
  for (const m of text.matchAll(re)) out.push(m[1])
  return out
}

/**
 * Resolve terraform's working dir: the `-chdir=<dir>` global option (only
 * valid before the subcommand), else `cwd`.
 * @param {string[]} argv
 * @param {string} cwd
 * @returns {string}
 */
export function terraformWorkDir(argv, cwd) {
  for (const a of argv) {
    if (!a.startsWith('-')) break // first positional = subcommand
    const m = /^--?chdir=(.*)$/.exec(a)
    if (m) return path.resolve(cwd, m[1])
  }
  return path.resolve(cwd)
}

/**
 * Scan a terraform working dir for code that runs outside Terraform.
 * @param {string} workDir
 * @returns {{detected: boolean, hits: string[]}} hits are `relpath:kind`
 */
export function scanTerraformExternalCode(workDir) {
  const root = path.resolve(workDir)
  const hits = []
  let detected = false
  let files = 0
  const visited = new Set()
  const label = (p) => path.relative(root, p) || path.basename(p)
  const hit = (p, kind) => {
    detected = true
    if (hits.length < MAX_HITS) hits.push(`${label(p)}:${kind}`)
  }

  // `recurseAll`: walk every subdir (used for .terraform/modules).
  const scanDir = (dir, depth, recurseAll) => {
    if (hits.length >= MAX_HITS) return
    if (depth > MAX_DEPTH) return hit(dir, 'scan-depth-limit')
    let real
    try {
      real = fs.realpathSync(dir)
    } catch {
      return hit(dir, 'unreadable')
    }
    if (visited.has(real)) return // cycle / shared module
    visited.add(real)
    let entries
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true })
    } catch {
      return hit(dir, 'unreadable')
    }
    for (const e of entries) {
      if (hits.length >= MAX_HITS) return
      const p = path.join(dir, e.name)
      if (e.isDirectory()) {
        if (recurseAll) scanDir(p, depth + 1, true)
        continue
      }
      const isJson = e.name.endsWith('.tf.json')
      if (!isJson && !e.name.endsWith('.tf')) continue
      if (++files > MAX_FILES) return hit(dir, 'scan-file-limit')
      let text
      try {
        text = fs.readFileSync(p, 'utf8')
      } catch {
        hit(p, 'unreadable')
        continue
      }
      for (const kind of isJson ? jsonKinds(text) : hclKinds(text)) hit(p, kind)
      for (const src of localSources(text, isJson)) {
        scanDir(path.resolve(path.dirname(p), src), depth + 1, false)
      }
    }
  }

  scanDir(root, 0, false)
  const downloaded = path.join(root, '.terraform', 'modules')
  if (fs.existsSync(downloaded)) scanDir(downloaded, 0, true)
  return { detected, hits }
}
