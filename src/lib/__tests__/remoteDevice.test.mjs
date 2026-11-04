// Unit tests for lib/remoteDevice.ts (Settings → Devices "This machine" card).
// Run: node --test src/lib/__tests__/remoteDevice.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  AUDIT_ACTION_LABELS, AUDIT_REASON_LABELS, LAST_ERROR_CODES, deviceState, errorReason,
  humanizeAction, nextSetupStep, setupDone, suggestAlias,
} from '../remoteDevice.ts'

const ready = {
  enabled: true,
  running: true,
  connected: true,
  reconnecting: false,
  relay_url: 'wss://relay.example.com/ws',
  has_auth_token: true,
  has_master_password: true,
  device_alias: 'macbook',
  session_authenticated: false,
  deviceConflict: null,
  last_error: null,
}

test('setup steps follow the relay, the name, the password and the switch', () => {
  assert.equal(nextSetupStep(null), 'relay')
  const blank = { ...ready, enabled: false, relay_url: '', has_auth_token: false, device_alias: null, has_master_password: false }
  assert.equal(nextSetupStep(blank), 'relay')
  // A relay URL without its token is not done.
  assert.equal(nextSetupStep({ ...blank, relay_url: 'wss://r' }), 'relay')
  assert.equal(nextSetupStep({ ...blank, relay_url: 'wss://r', has_auth_token: true }), 'name')
  assert.equal(nextSetupStep({ ...ready, enabled: false, has_master_password: false }), 'password')
  assert.equal(nextSetupStep({ ...ready, enabled: false }), 'enable')
  assert.equal(nextSetupStep(ready), null)
  assert.deepEqual(setupDone(ready), { relay: true, name: true, password: true, enable: true })
})

test('off: setup until only the switch is left', () => {
  assert.equal(deviceState(null), 'setup')
  assert.equal(deviceState({ ...ready, enabled: false, has_master_password: false }), 'setup')
  assert.equal(deviceState({ ...ready, enabled: false }), 'off')
})

test('enabled: relay state, conflicts and an attached phone', () => {
  assert.equal(deviceState(ready), 'ready')
  assert.equal(deviceState({ ...ready, connected: false }), 'connecting')
  assert.equal(deviceState({ ...ready, connected: false, reconnecting: true }), 'connecting')
  assert.equal(deviceState({ ...ready, connected: false, last_error: 'relay_auth_failed' }), 'error')
  assert.equal(deviceState({ ...ready, running: false, connected: false }), 'error')
  assert.equal(deviceState({ ...ready, session_authenticated: true }), 'phone')
  assert.equal(deviceState({ ...ready, deviceConflict: { kind: 'alias', alias: 'macbook' } }), 'aliasConflict')
  // A phone beats a name clash; a device id clash beats everything.
  assert.equal(
    deviceState({ ...ready, session_authenticated: true, deviceConflict: { kind: 'alias', alias: 'macbook' } }),
    'phone',
  )
  assert.equal(deviceState({ ...ready, session_authenticated: true, deviceConflict: { kind: 'device_id' } }), 'idConflict')
  assert.equal(deviceState({ ...ready, connected: false, last_error: 'device_id_conflict' }), 'idConflict')
})

test('errorReason maps known codes, else agent_stopped / unknown', () => {
  assert.equal(errorReason({ ...ready, last_error: 'relay_bad_url' }), 'relay_bad_url')
  assert.equal(errorReason({ ...ready, running: false }), 'agent_stopped')
  assert.equal(errorReason({ ...ready, last_error: 'something_new' }), 'unknown')
  assert.equal(errorReason(null), 'unknown')
})

test('suggestAlias offers the next free valid name', () => {
  assert.equal(suggestAlias('macbook'), 'macbook-2')
  assert.equal(suggestAlias('macbook-2'), 'macbook-3')
  assert.equal(suggestAlias('macbook', ['macbook-2', 'MACBOOK-3']), 'macbook-4')
  assert.equal(suggestAlias(''), 'devdy-2')
  assert.equal(suggestAlias('Mac Book!'), 'mac-book-2')
  const long = 'a'.repeat(32)
  const s = suggestAlias(long)
  assert.equal(s.length, 32)
  assert.ok(s.endsWith('-2'))
})

test('humanizeAction turns a wire action into words', () => {
  assert.equal(humanizeAction('send_chat_message'), 'Send chat message')
  assert.equal(humanizeAction('focus.board'), 'Focus board')
  assert.equal(humanizeAction(''), '')
})

test('every code and label has a message in both locales', () => {
  for (const loc of ['en', 'vi']) {
    const remote = JSON.parse(readFileSync(new URL(`../../i18n/locales/${loc}/remote.json`, import.meta.url), 'utf8'))
    for (const c of [...LAST_ERROR_CODES, 'agent_stopped', 'unknown']) {
      assert.ok(remote.device.errors[c], `${loc}: remote.device.errors.${c}`)
    }
    for (const a of AUDIT_ACTION_LABELS) assert.ok(remote.activity.actions[a], `${loc}: activity.actions.${a}`)
    for (const r of AUDIT_REASON_LABELS) assert.ok(remote.activity.reasons[r], `${loc}: activity.reasons.${r}`)
  }
})
