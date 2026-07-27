/**
 * TS side of the cross-language interop proof (spec §6, AC-19).
 *
 * Loads the SAME shared vector produced by the Rust impl
 * (`remote-e2e/vectors/basic.json`) and asserts the TS reference impl:
 *   1. derives the identical session key (both roles),
 *   2. reproduces the exact `cipher` when sealing with the fixed nonce
 *      (proves TS seal → Rust open, since bytes are identical),
 *   3. opens the vector `cipher` back to the plaintext
 *      (proves Rust seal → TS open),
 *   4. reproduces the host fingerprint,
 *   5. rejects a tampered cipher and a wrong psk.
 *
 * Run standalone (no vitest infra):
 *   npx tsx remote-e2e/interop/interop.mts
 * Exit code 0 = all interop assertions passed.
 *
 * This runner lives OUTSIDE the app's `src/**` typecheck scope on purpose: it
 * uses Node APIs (fs/path) that the browser-targeted app tsconfig does not
 * type. The E2E library it exercises (`src/remote/e2e/index.ts`) stays strict
 * and browser-clean.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import sodium from 'libsodium-wrappers'
import {
  Session,
  deriveSession,
  generateKeypair,
  hostFingerprint,
  keypairFromBytes,
  ready,
} from '../../src/remote/e2e/index.ts'

interface Vector {
  readonly psk_hex: string
  readonly sk_a_hex: string
  readonly pk_a_hex: string
  readonly sk_b_hex: string
  readonly pk_b_hex: string
  readonly nonce_hex: string
  readonly plaintext_utf8: string
  readonly session_key_hex: string
  readonly host_fingerprint_hex: string
  readonly cipher_b64: string
}

function loadVector(): Vector {
  const here = dirname(fileURLToPath(import.meta.url))
  const path = resolve(here, '../vectors/basic.json')
  return JSON.parse(readFileSync(path, 'utf8')) as Vector
}

let failures = 0
function check(name: string, cond: boolean): void {
  if (cond) {
    console.log(`  ok  - ${name}`)
  } else {
    failures += 1
    console.error(`  FAIL- ${name}`)
  }
}

function eqBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false
  return true
}

async function main(): Promise<void> {
  await ready()
  const v = loadVector()

  const psk = sodium.from_hex(v.psk_hex)
  const a = keypairFromBytes(sodium.from_hex(v.pk_a_hex), sodium.from_hex(v.sk_a_hex))
  const b = keypairFromBytes(sodium.from_hex(v.pk_b_hex), sodium.from_hex(v.sk_b_hex))

  // 1. Session key matches vector, both roles.
  const sa = deriveSession(psk, a, b.publicKey)
  const sb = deriveSession(psk, b, a.publicKey)
  check('session key equal for both roles', eqBytes(sa.keyBytes(), sb.keyBytes()))
  check('session key matches vector', sodium.to_hex(sa.keyBytes()) === v.session_key_hex)

  // 2. Deterministic seal reproduces the vector cipher (TS seal → Rust bytes).
  const nonce = sodium.from_hex(v.nonce_hex)
  const plaintext = sodium.from_string(v.plaintext_utf8)
  const cipher = sa.sealWithNonce(plaintext, nonce)
  check('TS seal reproduces vector cipher', cipher === v.cipher_b64)

  // 3. Open the vector cipher (Rust seal → TS open).
  const opened = sb.open(v.cipher_b64)
  check('TS opens vector cipher to plaintext', sodium.to_string(opened) === v.plaintext_utf8)

  // 4. Fingerprint matches.
  check('host fingerprint matches vector', hostFingerprint(a.publicKey) === v.host_fingerprint_hex)

  // 5. Tamper + wrong-psk rejection.
  let tamperRejected = false
  try {
    const bad = cipher.slice(0, -2) + (cipher.endsWith('A=') ? 'B=' : 'A=')
    sb.open(bad)
  } catch {
    tamperRejected = true
  }
  check('tampered cipher rejected', tamperRejected)

  let wrongPskRejected = false
  try {
    const sWrong = deriveSession(sodium.from_string('the-wrong-psk-entirely-nope'), b, a.publicKey)
    sWrong.open(v.cipher_b64)
  } catch {
    wrongPskRejected = true
  }
  check('wrong psk rejected', wrongPskRejected)

  // 6. Fresh-ephemeral round trip (forward-secrecy path).
  const host = generateKeypair()
  const ctrl = generateKeypair()
  const sHost = Session.derive(sodium.from_string('a-strong-pairing-psk-32-bytes!!!'), host, ctrl.publicKey)
  const sCtrl = Session.derive(sodium.from_string('a-strong-pairing-psk-32-bytes!!!'), ctrl, host.publicKey)
  const msg = sodium.from_string('{"kind":"cmd","action":"start_run","run_id":"r_2"}')
  const rt = sHost.open(sCtrl.seal(msg))
  check('fresh-ephemeral round trip', sodium.to_string(rt) === '{"kind":"cmd","action":"start_run","run_id":"r_2"}')

  if (failures > 0) {
    console.error(`\nINTEROP FAILED: ${failures} assertion(s) failed`)
    process.exit(1)
  }
  console.log('\nINTEROP OK: all TS assertions passed against shared Rust vector')
}

main().catch((err: unknown) => {
  console.error('interop crashed:', err)
  process.exit(1)
})
