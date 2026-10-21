// Unit tests for the terraform external-code scanner.
// Run with: node --test sidecar-proxy/terraform-scan.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { scanTerraformExternalCode, terraformWorkDir } from './terraform-scan.mjs'

function fixture(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'devdy-tfscan-'))
  for (const [rel, content] of Object.entries(files)) {
    const p = path.join(root, rel)
    fs.mkdirSync(path.dirname(p), { recursive: true })
    fs.writeFileSync(p, content)
  }
  return root
}

test('clean config → not detected', () => {
  const root = fixture({
    'main.tf': 'provider "aws" {}\nresource "aws_s3_bucket" "b" { bucket = "x" }\n',
    'versions.tf': 'terraform { required_providers { aws = { source = "hashicorp/aws" } } }\n',
  })
  assert.deepEqual(scanTerraformExternalCode(root), { detected: false, hits: [] })
})

test('data "external" → detected with file:kind hit', () => {
  const root = fixture({ 'main.tf': 'data "external" "x" { program = ["sh", "-c", "env"] }\n' })
  const r = scanTerraformExternalCode(root)
  assert.equal(r.detected, true)
  assert.deepEqual(r.hits, ['main.tf:data.external'])
})

test('data "http" and .tf.json forms', () => {
  const root = fixture({
    'a.tf': 'data   "http" "ip" { url = "https://x" }\n',
    'b.tf.json': '{"data": {"external": {"x": {"program": ["env"]}}}}',
  })
  const r = scanTerraformExternalCode(root)
  assert.equal(r.detected, true)
  assert.ok(r.hits.includes('a.tf:data.http'))
  assert.ok(r.hits.includes('b.tf.json:data.external'))
})

test('local module with local-exec → detected (incl. ../ sources and cycles)', () => {
  const root = fixture({
    'env/main.tf': 'module "net" {\n  source = "../modules/net"\n}\n',
    'modules/net/main.tf': 'module "self" { source = "./" }\nmodule "loop" { source = "../../env" }\n',
    'modules/net/null.tf':
      'resource "null_resource" "r" {\n  provisioner "local-exec" { command = "env" }\n}\n',
  })
  const r = scanTerraformExternalCode(path.join(root, 'env'))
  assert.equal(r.detected, true)
  assert.deepEqual(r.hits, ['../modules/net/null.tf:local-exec'])
})

test('downloaded modules under .terraform/modules are scanned', () => {
  const root = fixture({
    'main.tf': 'module "x" { source = "terraform-aws-modules/x/aws" }\n',
    '.terraform/modules/x/sub/run.tf': 'data "external" "e" { program = ["id"] }\n',
  })
  const r = scanTerraformExternalCode(root)
  assert.equal(r.detected, true)
  assert.deepEqual(r.hits, ['.terraform/modules/x/sub/run.tf:data.external'])
})

test('-chdir handling', () => {
  assert.equal(terraformWorkDir(['-chdir=envs/dev', 'plan'], '/repo'), '/repo/envs/dev')
  assert.equal(terraformWorkDir(['-chdir=/abs', 'plan'], '/repo'), '/abs')
  assert.equal(terraformWorkDir(['plan', '-chdir=ignored'], '/repo'), '/repo')
  assert.equal(terraformWorkDir([], '/repo'), '/repo')
  const root = fixture({ 'envs/dev/main.tf': 'data "external" "x" { program = ["id"] }\n' })
  const r = scanTerraformExternalCode(terraformWorkDir(['-chdir=envs/dev', 'plan'], root))
  assert.equal(r.detected, true)
})

test('unreadable or missing dir/file → detected (fail-closed)', () => {
  const missing = scanTerraformExternalCode(path.join(os.tmpdir(), 'devdy-tfscan-missing-xyz'))
  assert.equal(missing.detected, true)
  const root = fixture({ 'main.tf': 'resource "x" "y" {}\n' })
  fs.chmodSync(path.join(root, 'main.tf'), 0o000)
  try {
    const r = scanTerraformExternalCode(root)
    // root can still read files when running as root; only assert when denied.
    if (process.getuid && process.getuid() !== 0) {
      assert.equal(r.detected, true)
      assert.deepEqual(r.hits, ['main.tf:unreadable'])
    }
  } finally {
    fs.chmodSync(path.join(root, 'main.tf'), 0o644)
  }
  const dangling = fixture({ 'main.tf': 'module "m" { source = "./nope" }\n' })
  assert.equal(scanTerraformExternalCode(dangling).detected, true)
})
