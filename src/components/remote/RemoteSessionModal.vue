<script setup lang="ts">
// Per-run remote-control session modal. A small state machine that walks the
// Owner through: create link → controller scans → OTP shown → connected.
//
// Flow:
//   creating          → invoke createSessionLink(runId); spinner
//   waiting-for-scan   → show QR + copy link; wait for controller to join
//   controller-joined  → controller requested a session; show the OTP large
//   connected          → controller finished the OTP handshake; live control
//   auth-failed        → wrong OTP; show reason + attempts; allow regenerate
//   expired / error    → offer to create a fresh link
//
// Secrets: the controller link carries `link_secret` in the URL HASH so it
// never hits relay/server logs. The OTP is shown out-of-band on this screen.
import { ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Modal, Button } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore, type SessionLink } from '@/stores/remoteControl'
import PairingQr from './PairingQr.vue'
import {
  Radio, Copy, Check, Loader2, ShieldCheck, ShieldAlert, RotateCcw,
} from 'lucide-vue-next'

const props = defineProps<{
  open: boolean
  runId: string | null
}>()

const emit = defineEmits<{ close: [] }>()

const store = useRemoteControlStore()
const { t } = useI18n()
const { toast } = useToast()

type Phase =
  | 'creating'
  | 'waiting-for-scan'
  | 'controller-joined'
  | 'connected'
  | 'auth-failed'
  | 'expired'
  | 'error'

const phase = ref<Phase>('creating')
const link = ref<SessionLink | null>(null)
const linkUrl = ref('')
const errorMsg = ref('')
const copied = ref(false)

// Auth state (from remote://controller-requested-session / auth-failed). The OTP
// itself is no longer shown in this modal — it is still auto-generated on the
// host as the fallback secret, but pairing is done with the master password.
const attemptsLeft = ref<number | null>(null)
const authReason = ref('')

// Idle expiry of the live session token (unix secs), shown on the connected view.
const sessionIdleExpiresAt = ref<number | null>(null)
const idleRemainingLabel = computed(() => {
  if (sessionIdleExpiresAt.value == null) return null
  const secs = Math.max(0, sessionIdleExpiresAt.value - now.value)
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  return h > 0 ? `${h}h ${m}m` : `${m}m`
})

// Clock driving the connected view's idle-expiry countdown.
const now = ref(Math.floor(Date.now() / 1000))
let clockTimer: ReturnType<typeof setInterval> | null = null

const unlisteners: UnlistenFn[] = []
let copyTimer: ReturnType<typeof setTimeout> | null = null

// ── Controller link URL ────────────────────────────────────────────────────
// The controller web app is served at `controller.html` next to the relay
// endpoint. We derive its base from the relay URL (wss://host[:port]/...) by
// swapping the scheme to https and replacing the path with `/controller.html`.
// ALL link fields go into the URL HASH so the secrets never reach server logs.
//
//   https://<relay-host>[:port]/controller.html#relay_url=<enc>&host_fingerprint=<hex>
//     &rendezvous_code=<code>&run_id=<id>&link_secret=<hex>
//
// If the relay URL cannot be parsed we leave `linkUrl` empty and fall back to
// showing the raw fields for manual copy (see the template).
function buildLinkUrl(l: SessionLink): string {
  try {
    const u = new URL(l.relay_url)
    const scheme = u.protocol === 'ws:' ? 'http:' : 'https:'
    const base = `${scheme}//${u.host}/controller.html`
    const hash = new URLSearchParams({
      relay_url: l.relay_url,
      host_fingerprint: l.host_fingerprint,
      rendezvous_code: l.rendezvous_code,
      run_id: l.run_id,
      link_secret: l.link_secret,
    }).toString()
    return `${base}#${hash}`
  } catch {
    return ''
  }
}

async function copyLinkOrFields() {
  const payload = linkUrl.value || rawFields.value
  try {
    await navigator.clipboard.writeText(payload)
    copied.value = true
    if (copyTimer) clearTimeout(copyTimer)
    copyTimer = setTimeout(() => (copied.value = false), 2000)
  } catch {
    toast.error(t('remote.session.copyFailed'))
  }
}

// Raw fields fallback (when the base URL can't be derived). Same data as the
// hash, one field per line, so it can still be pasted into the controller.
const rawFields = computed(() => {
  const l = link.value
  if (!l) return ''
  return [
    `relay_url=${l.relay_url}`,
    `host_fingerprint=${l.host_fingerprint}`,
    `rendezvous_code=${l.rendezvous_code}`,
    `run_id=${l.run_id}`,
    `link_secret=${l.link_secret}`,
  ].join('\n')
})

// ── Session lifecycle ───────────────────────────────────────────────────────
async function createLink() {
  if (!props.runId) {
    phase.value = 'error'
    errorMsg.value = t('remote.session.noRunOpen')
    return
  }
  phase.value = 'creating'
  errorMsg.value = ''
  attemptsLeft.value = null
  authReason.value = ''
  try {
    const l = await store.createSessionLink(props.runId)
    link.value = l
    linkUrl.value = buildLinkUrl(l)
    phase.value = 'waiting-for-scan'
  } catch (e) {
    phase.value = 'error'
    errorMsg.value = String(e)
  }
}

/** Decide the initial screen when the modal opens. If this run already has a
 * live (authenticated) remote session, show its status + a stop control instead
 * of superseding it with a brand-new link. Otherwise create a fresh link. */
async function openFlow() {
  if (!props.runId) {
    phase.value = 'error'
    errorMsg.value = t('remote.session.noRunOpen')
    return
  }
  try {
    const s = await store.refreshStatus()
    if (s.bound_run_id === props.runId && s.session_authenticated) {
      sessionIdleExpiresAt.value = s.session_idle_expires_at
      phase.value = 'connected'
      return
    }
  } catch {
    // Status unavailable — fall through to create a fresh link.
  }
  await createLink()
}

async function stopControl() {
  try {
    await store.endSession()
  } catch {
    // best-effort
  }
  emit('close')
}

function matchesRun(payloadRunId: string): boolean {
  return !!props.runId && payloadRunId === props.runId
}

async function subscribe() {
  unlisteners.push(
    await listen<{ run_id: string; otp: string; otp_expires_at: number; attempts_left: number }>(
      'remote://controller-requested-session',
      (e) => {
        if (!matchesRun(e.payload.run_id)) return
        // A controller is connecting. The OTP is no longer shown here (pairing
        // uses the master password); we only track attempts for the failure view.
        attemptsLeft.value = e.payload.attempts_left
        authReason.value = ''
        phase.value = 'controller-joined'
      },
    ),
  )
  unlisteners.push(
    await listen<{ run_id: string }>('remote://connection-authenticated', (e) => {
      if (!matchesRun(e.payload.run_id)) return
      phase.value = 'connected'
      // Refresh so the connected view can show the idle expiry.
      store.refreshStatus().then((s) => {
        if (s.bound_run_id === props.runId) sessionIdleExpiresAt.value = s.session_idle_expires_at
      }).catch(() => {})
    }),
  )
  unlisteners.push(
    await listen<{ run_id: string; attempts_left: number; reason: string }>(
      'remote://auth-failed',
      (e) => {
        if (!matchesRun(e.payload.run_id)) return
        attemptsLeft.value = e.payload.attempts_left
        authReason.value = e.payload.reason
        phase.value = 'auth-failed'
      },
    ),
  )
  unlisteners.push(
    await listen<{ run_id: string }>('remote://disconnected', (e) => {
      if (!matchesRun(e.payload.run_id)) return
      // Controller dropped — go back to waiting so a fresh join can happen.
      if (phase.value === 'connected' || phase.value === 'controller-joined') {
        phase.value = 'waiting-for-scan'
      }
    }),
  )
  unlisteners.push(
    await listen<{ run_id: string }>('remote://session-expired', (e) => {
      if (!matchesRun(e.payload.run_id)) return
      phase.value = 'expired'
    }),
  )
}

onMounted(async () => {
  clockTimer = setInterval(() => {
    now.value = Math.floor(Date.now() / 1000)
  }, 1000)
  await subscribe()
  await openFlow()
})

onBeforeUnmount(() => {
  if (clockTimer) clearInterval(clockTimer)
  if (copyTimer) clearTimeout(copyTimer)
  for (const un of unlisteners) {
    try {
      un()
    } catch {
      // ignore
    }
  }
})

// Close just dismisses the modal — it does NOT end the remote session. The
// session keeps running so the phone stays connected; the user reopens this
// modal (Remote button) to see status or stop it explicitly.
function onClose() {
  emit('close')
}

// If the run changes underneath us, just close — the parent rebinds per run.
watch(() => props.runId, () => {
  emit('close')
})
</script>

<template>
  <Modal :open="open" :title="t('remote.session.title')" size="sm" @close="onClose">
    <template #header>
      <Radio class="h-4 w-4 text-primary shrink-0" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('remote.session.heading') }}</h3>
    </template>

    <div class="p-6">
      <!-- creating -->
      <div v-if="phase === 'creating'" class="flex flex-col items-center gap-3 py-8">
        <Loader2 class="h-6 w-6 animate-spin text-primary" :stroke-width="2" />
        <p class="text-sm text-muted-foreground">{{ t('remote.session.creating') }}</p>
      </div>

      <!-- waiting-for-scan -->
      <div v-else-if="phase === 'waiting-for-scan'" class="space-y-4">
        <PairingQr
          v-if="linkUrl"
          :text="linkUrl"
          :ttl-seconds="120"
          :copy-label="t('remote.session.copyLink')"
          :caption="t('remote.session.scanCaption')"
          @expired="createLink"
        />
        <div v-else class="space-y-2">
          <p class="text-xs text-amber-500">
            {{ t('remote.session.cannotDeriveController') }}
          </p>
          <pre class="rounded-md bg-muted/40 p-2 text-[11px] font-mono whitespace-pre-wrap break-all">{{ rawFields }}</pre>
          <Button variant="outline" size="sm" @click="copyLinkOrFields">
            <Check v-if="copied" class="h-3.5 w-3.5 text-emerald-500" :stroke-width="2" />
            <Copy v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ copied ? t('remote.session.copied') : t('remote.session.copyFields') }}
          </Button>
        </div>

        <p class="text-center text-xs text-muted-foreground">
          <i18n-t keypath="remote.session.enterMasterOnPhone" tag="span">
            <template #masterPassword>
              <span class="font-medium text-foreground/80">{{ t('remote.session.masterPassword') }}</span>
            </template>
          </i18n-t>
        </p>
      </div>

      <!-- controller-joined: a controller is authenticating (no OTP shown) -->
      <div v-else-if="phase === 'controller-joined'" class="flex flex-col items-center gap-3 py-6">
        <Loader2 class="h-6 w-6 animate-spin text-primary" :stroke-width="2" />
        <p class="text-sm text-muted-foreground">{{ t('remote.session.connecting') }}</p>
        <p class="text-center text-xs text-muted-foreground">
          {{ t('remote.session.enterMasterToFinish') }}
        </p>
      </div>

      <!-- connected -->
      <div v-else-if="phase === 'connected'" class="flex flex-col items-center gap-4 py-6">
        <span class="relative flex h-3 w-3">
          <span class="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-75 animate-ping" />
          <span class="relative inline-flex h-3 w-3 rounded-full bg-emerald-500" />
        </span>
        <ShieldCheck class="h-8 w-8 text-emerald-500" :stroke-width="1.75" />
        <p class="text-sm font-medium text-emerald-500">{{ t('remote.session.connectedLive') }}</p>
        <p class="text-center text-xs text-muted-foreground">
          {{ t('remote.session.phoneControlling') }}
        </p>
        <p v-if="idleRemainingLabel" class="text-center text-[11px] text-muted-foreground">
          {{ t('remote.session.idleExpiry', { time: idleRemainingLabel }) }}
        </p>
        <Button variant="destructive" @click="stopControl">{{ t('remote.session.stopControl') }}</Button>
      </div>

      <!-- auth-failed -->
      <div v-else-if="phase === 'auth-failed'" class="flex flex-col items-center gap-4 py-6">
        <ShieldAlert class="h-8 w-8 text-destructive" :stroke-width="1.75" />
        <p class="text-sm font-medium text-destructive">{{ t('remote.session.authFailed') }}</p>
        <div class="text-center text-xs text-muted-foreground space-y-0.5">
          <p v-if="authReason">{{ authReason }}</p>
          <p v-if="attemptsLeft != null">{{ t('remote.session.attemptsLeft', { count: attemptsLeft }) }}</p>
        </div>
        <Button variant="outline" @click="createLink">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('remote.session.createNewLink') }}
        </Button>
      </div>

      <!-- expired / error -->
      <div v-else class="flex flex-col items-center gap-4 py-6">
        <ShieldAlert class="h-8 w-8 text-amber-500" :stroke-width="1.75" />
        <p class="text-sm font-medium text-foreground">
          {{ phase === 'expired' ? t('remote.session.expired') : t('remote.session.errorOccurred') }}
        </p>
        <p v-if="errorMsg" class="text-center text-xs text-destructive">{{ errorMsg }}</p>
        <Button variant="outline" @click="createLink">
          <RotateCcw class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('remote.session.createNewLink') }}
        </Button>
      </div>
    </div>

    <template #footer>
      <Button variant="outline" @click="onClose">{{ t('common.close') }}</Button>
    </template>
  </Modal>
</template>
