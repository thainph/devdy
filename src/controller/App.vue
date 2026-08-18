<script setup lang="ts">
/**
 * Devdy Remote Controller — root app (Phase 4). A standalone, Tauri-free web
 * client that controls ONE run over the relay. Flow:
 *   1. A valid stored token → auto-reattach (skip entry) → SessionView.
 *   2. Otherwise: LinkEntryView → connect + wait for the Host OTP gate →
 *      OtpEntryView → on success (session_token) → SessionView.
 * The single-run focus screen mirrors the desktop session view.
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useMarkdown } from '@/lib/markdown'
import { mergeContextModel } from '@/lib/contextLimits'
import LinkEntryView from './components/LinkEntryView.vue'
import OtpEntryView from './components/OtpEntryView.vue'
import SessionView from './components/SessionView.vue'
import type { ComposerTurn } from './components/SessionComposer.vue'
import { ControllerConnection, type AuthState, type ConnStatus, type SecretSource } from './connection'
import { createRunStore } from './runStore'
import * as tokenStore from './sessionTokenStore'
import {
  cancelRunCmd,
  listProjectFilesCmd,
  parseSessionLink,
  respondPermissionCmd,
  sendTurnCmd,
  setRunMetaCmd,
  type QuestionAnswers,
  type RemoteDecision,
  type SessionLinkPayload,
} from './protocol'

const { t } = useI18n()
const { renderText, loadMarkdown } = useMarkdown()

const store = createRunStore()
const conn = shallowRef<ControllerConnection | null>(null)

/** The app phase drives which screen shows. */
type Phase = 'link' | 'otp' | 'session'
const phase = ref<Phase>('link')

const link = ref<SessionLinkPayload | null>(null)

// Mirrored connection state (local refs for the template).
const status = ref<ConnStatus>('idle')
const hostFp = ref<string | null>(null)
const lastError = ref<string | null>(null)
const live = ref(false)
const decodeErrors = ref(0)
const auth = ref<AuthState | null>(null)

// Keep connection-state watchers in the root component scope. Creating these
// inside `onLink` would associate them with the LinkEntryView emit scope; they
// would then stop as soon as that view unmounts for the OTP screen.
watch(
  () => conn.value?.state.status.value ?? 'idle',
  (v) => (status.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.hostFingerprint.value ?? null,
  (v) => (hostFp.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.lastError.value ?? null,
  (v) => (lastError.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.live.value ?? false,
  (v) => (live.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.decodeErrors.value ?? 0,
  (v) => (decodeErrors.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.auth.value ?? null,
  (v) => {
    auth.value = v
    if (v === 'authenticated') phase.value = 'session'
    // On failure stay on the OTP screen so the user can retry (unless we are
    // reconnecting with a token — then fall back to OTP entry).
    else if (v === 'failed') phase.value = 'otp'
  },
  { immediate: true },
)

// Composer selectors. These are kept in lock-step with the desktop: the Host
// pushes its live selection via `run_meta` (mirrored below) and a local edit is
// pushed straight back with `set_run_meta`. `applyingMeta` suppresses the echo so
// a mirrored value doesn't bounce back to the Host.
const engine = ref('')
const model = ref('')
const permissionMode = ref('')
let applyingMeta = false
watch(
  () => store.state.runMeta,
  (m) => {
    applyingMeta = true
    engine.value = m.engine
    model.value = m.model
    permissionMode.value = m.permissionMode
    void nextTick(() => {
      applyingMeta = false
    })
  },
  { deep: true, immediate: true },
)
function pushMeta(): void {
  if (applyingMeta) return
  const id = runId.value
  if (!id) return
  conn.value?.sendCommand(
    setRunMetaCmd(id, {
      engine: engine.value,
      model: model.value,
      permissionMode: permissionMode.value,
    }),
  )
}
function onEngine(v: string): void {
  engine.value = v
  pushMeta()
}
function onModel(v: string): void {
  model.value = v
  pushMeta()
}
function onPermissionMode(v: string): void {
  permissionMode.value = v
  pushMeta()
}

// Whether the bound run is currently executing. We flip it optimistically on
// send and clear it on a `done`.
const running = ref(false)
const sending = ref(false)

onMounted(() => {
  loadMarkdown()
  const token = tokenStore.load()
  const hashLink = parseHashLink()
  // A URL link for a DIFFERENT session (different host fingerprint or run) is an
  // explicit intent to connect THERE — it must override a stale stored token.
  // Otherwise we'd auto-reattach to the OLD run and fail with a confusing
  // "fingerprint mismatch / MITM" error, never reaching the session in the link.
  if (
    hashLink &&
    token &&
    (hashLink.host_fingerprint !== token.host_fingerprint || hashLink.run_id !== token.run_id)
  ) {
    tokenStore.clear()
    onLink(hashLink)
    return
  }
  // Token reconnect: a still-fresh token (for the same session) skips the OTP.
  if (token) {
    link.value = {
      relay_url: token.relay_url,
      host_fingerprint: token.host_fingerprint,
      rendezvous_code: token.rendezvous_code,
      run_id: token.run_id,
      link_secret: token.link_secret,
    }
    tokenStore.touch()
    startConnection({ mode: 'session', token: token.token })
    phase.value = 'session'
  }
  // No token → stay on LinkEntryView (phase 'link'), which parses the hash itself.
})

/** Parse a session link from the URL hash (mirrors LinkEntryView), or null. */
function parseHashLink(): SessionLinkPayload | null {
  const hash = window.location.hash.replace(/^#/, '')
  if (!hash) return null
  try {
    return parseSessionLink(
      hash.includes('=') || hash.startsWith('{') ? hash : decodeURIComponent(hash),
    )
  } catch {
    return null
  }
}

onBeforeUnmount(() => {
  conn.value?.close()
})

/** Parsed a link (from hash or paste) — connect IMMEDIATELY (join + pubkey
 * handshake) so the desktop shows its OTP, then show the OTP entry screen. The
 * session key is derived later, once the user submits the OTP. */
function onLink(payload: SessionLinkPayload): void {
  link.value = payload
  phase.value = 'otp'
  startConnection(null)
}

/** The user submitted an OTP — derive the key + fire the gate on the live
 * connection (safe to call again to retry a wrong code). */
function onOtp(otp: string): void {
  if (!link.value) return
  if (!conn.value) startConnection(null)
  conn.value?.submitOtp(otp)
}

/** Build a connection for the current link + secret source and wire its refs.
 * `secret` is null for a fresh OTP link (the OTP arrives later via submitOtp). */
function startConnection(secret: SecretSource | null): void {
  if (!link.value) return
  conn.value?.close()
  store.reset()
  running.value = false
  sending.value = false
  const c = new ControllerConnection(link.value, secret, (p) => store.apply(p))
  conn.value = c
  void c.connect()
}

function disconnect(): void {
  tokenStore.clear()
  conn.value?.close()
  conn.value = null
  store.reset()
  link.value = null
  phase.value = 'link'
  status.value = 'idle'
  hostFp.value = null
  lastError.value = null
  live.value = false
  decodeErrors.value = 0
  auth.value = null
  running.value = false
}

// ── bound run view ──────────────────────────────────────────────────────────
const runId = computed(() => link.value?.run_id ?? null)
const runView = computed(() => (runId.value ? store.state.runs[runId.value] ?? null : null))
const entries = computed(() => runView.value?.entries ?? [])
// Usage / context metrics derived from the stream (desktop parity).
const contextTokens = computed(() => runView.value?.contextTokens ?? 0)
// `system.init` drops the `[1m]` suffix; re-attach it from the mirrored composer
// selection so 1M runs resolve to the 1M context limit rather than 200K.
const contextModel = computed(() =>
  mergeContextModel(runView.value?.model ?? null, store.state.runMeta.model),
)
const rateLimit = computed(() => runView.value?.rateLimit ?? null)
const usage = computed(() => runView.value?.usage ?? null)
// Subscription plan-usage badges (Claude + Codex), pushed by the Host.
const claudeBudget = computed(() => store.state.claudeBudget)
const codexBudget = computed(() => store.state.codexBudget)

// Clear the running flag when the run reports a terminal status.
watch(
  () => (runId.value ? store.state.runs[runId.value]?.status : null),
  (s) => {
    if (s === 'running') running.value = true
    else if (s && ['done', 'cancelled', 'failed', 'completed', 'error', 'fetched'].includes(s)) {
      running.value = false
    }
    sending.value = false
  },
  { immediate: true },
)

// Prefer the session's real title (from the Host's run-list snapshot, sent on
// pairing and refreshed when the run finishes). Fall back to a short run id when
// the run has not been titled yet.
const title = computed(() => {
  const id = runId.value
  if (!id) return 'Run'
  const info = store.state.runList.find((r) => r.id === id)
  const t = info?.title?.trim()
  return t ? t : `Run ${id.slice(0, 8)}`
})

// The OTP screen's inline error: only while auth actually failed (not a
// transient transport error), so we don't scare the user on a reconnect blip.
const otpError = computed(() => (auth.value === 'failed' ? lastError.value : null))
const otpConnecting = computed(
  () =>
    auth.value === 'pending' ||
    status.value === 'joining' ||
    (status.value === 'handshaking' && !hostFp.value),
)
const shortFp = computed(() => (link.value ? link.value.host_fingerprint.slice(0, 12) : ''))

// ── composer / permission wiring ────────────────────────────────────────────
function warnNotSent(): void {
  store.state.notice = t('controller.app.notConnected')
}

function onSend(turn: ComposerTurn): void {
  const id = runId.value
  if (!id) return
  const cmd = sendTurnCmd(
    id,
    {
      text: turn.text,
      engine: engine.value,
      model: model.value,
      permissionMode: permissionMode.value,
      mentions: turn.mentions,
      attachments: turn.attachments,
    },
    running.value,
  )
  const sent = conn.value?.sendCommand(cmd, (accepted, reason) => {
    sending.value = false
    if (!accepted) {
      store.state.notice =
        reason === 'connection_lost' || reason === 'disconnected'
          ? t('controller.app.connectionLost')
          : reason
            ? t('controller.app.hostRejectedReason', { reason })
            : t('controller.app.hostRejected')
      return
    }
    turn.accept()
    // Echo only after the Host confirms it accepted the command. This prevents
    // a reconnect race from showing (then losing) a turn the Host never saw.
    store.pushUser(
      id,
      turn.text,
      turn.attachments.map((a) => ({ media_type: a.mime, data: a.data_base64 })),
    )
    running.value = true
  })
  if (sent) {
    sending.value = true
  } else {
    sending.value = false
    warnNotSent()
  }
}

function onCancel(): void {
  const id = runId.value
  if (!id) return
  if (!conn.value?.sendCommand(cancelRunCmd(id))) warnNotSent()
}

function onDecide(decision: RemoteDecision): void {
  const req = store.state.pending
  if (!req) return
  if (!conn.value?.sendCommand(respondPermissionCmd(req.run_id, req.request_id, decision))) {
    warnNotSent()
    return
  }
  // Keep the prompt until the Host confirms the sidecar/broker accepted it.
}

function onAnswer(answers: QuestionAnswers): void {
  const req = store.state.pending
  if (!req) return
  if (!conn.value?.sendCommand(respondPermissionCmd(req.run_id, req.request_id, 'allow_once', answers))) {
    warnNotSent()
    return
  }
  // Keep the prompt until the Host confirms the sidecar/broker accepted it.
}

function onRequestFiles(): void {
  const id = runId.value
  if (id) conn.value?.sendCommand(listProjectFilesCmd(id))
}
</script>

<template>
  <LinkEntryView v-if="phase === 'link'" @link="onLink" />

  <OtpEntryView
    v-else-if="phase === 'otp'"
    :short-fingerprint="shortFp"
    :connecting="otpConnecting"
    :error="otpError"
    @otp="onOtp"
  />

  <SessionView
    v-else
    :title="title"
    :status="status"
    :live="live"
    :host-fingerprint="hostFp"
    :decode-errors="decodeErrors"
    :last-error="lastError"
    :notice="store.state.notice"
    :entries="entries"
    :running="running"
    :pending="store.state.pending"
    :render-text="renderText"
    :slash-commands="store.state.slashCommands"
    :project-files="store.state.projectFiles"
    :engine="engine"
    :model="model"
    :permission-mode="permissionMode"
    :sending="sending"
    :context-tokens="contextTokens"
    :context-model="contextModel"
    :rate-limit="rateLimit"
    :usage="usage"
    :claude-budget="claudeBudget"
    :codex-budget="codexBudget"
    @send="onSend"
    @cancel="onCancel"
    @decide="onDecide"
    @answer="onAnswer"
    @disconnect="disconnect"
    @clear-notice="store.clearNotice()"
    @update:engine="onEngine"
    @update:model="onModel"
    @update:permission-mode="onPermissionMode"
    @request-files="onRequestFiles"
  />
</template>
