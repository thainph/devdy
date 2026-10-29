/**
 * Password login (no link): reach the Host through its well-known login slot
 * with only the owner's master password, and get back a full session link +
 * token. The caller then continues on the normal session path.
 *
 * The E2E key is derived from the password (see `src/lib/remoteLogin.ts`), so a
 * wrong password simply means no frame ever opens — the Host stays silent and
 * this times out. There is no pinned fingerprint on this path; the password
 * itself authenticates the Host, and the granted link pins it from then on.
 */
import { ready as e2eReady, generateKeypair, deriveEffectivePsk } from '@/remote/e2e'
import { LOGIN_LINK_SECRET, loginCodeFor, sameOriginRelayUrl, stretchPassword } from '@/lib/remoteLogin'
import {
  base64ToUtf8,
  cmdFrame,
  controlFrame,
  fromHex,
  hsAnswerJson,
  joinFrame,
  parseHsOffer,
  toHex,
  utf8ToBase64,
  type Envelope,
  type SessionLinkPayload,
} from './protocol'
import { deriveSessionKey } from './handshake'

/** Long enough for PBKDF2 on a slow phone plus the Host's own check. */
const LOGIN_TIMEOUT_MS = 20_000

export type LoginErrorCode = 'host_offline' | 'wrong_password' | 'network'

export class LoginError extends Error {
  constructor(readonly code: LoginErrorCode) {
    super(code)
  }
}

export interface LoginGrant {
  link: SessionLinkPayload
  token: string
}

export interface LoginOptions {
  /** Relay WebSocket URL; defaults to the relay serving this page. */
  relayUrl?: string
  /** Alias of the Host to log in to (selects its login code); none = unnamed. */
  device?: string
}

/** Log in with `password`. Resolves with the granted link + token. */
export async function loginWithPassword(password: string, opts: LoginOptions = {}): Promise<LoginGrant> {
  await e2eReady()
  const relayUrl = opts.relayUrl ?? sameOriginRelayUrl(window.location)
  const loginCode = loginCodeFor(opts.device)
  const psk = deriveEffectivePsk(LOGIN_LINK_SECRET, await stretchPassword(password))

  return new Promise<LoginGrant>((resolve, reject) => {
    let ws: WebSocket
    try {
      ws = new WebSocket(relayUrl)
    } catch {
      reject(new LoginError('network'))
      return
    }
    let roomId: string | null = null
    let session: ReturnType<typeof deriveSessionKey>['session'] | null = null
    let seq = 0
    let settled = false

    const finish = (err: LoginError | null, grant?: LoginGrant): void => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null
      try {
        ws.close()
      } catch {
        // already closing
      }
      if (err) reject(err)
      else resolve(grant!)
    }
    // Silence after our sealed frame = the Host could not open it.
    const timer = setTimeout(
      () => finish(new LoginError(session ? 'wrong_password' : 'host_offline')),
      LOGIN_TIMEOUT_MS,
    )
    const send = (env: Envelope): void => {
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(env))
    }

    ws.onopen = () => send(joinFrame(loginCode))
    ws.onerror = () => {}
    ws.onclose = () => finish(new LoginError('network'))
    ws.onmessage = (ev) => {
      let env: Envelope
      try {
        env = JSON.parse(String(ev.data)) as Envelope
      } catch {
        return
      }
      if (env.t === 'room_ready' && env.room_id) {
        roomId = env.room_id
        return
      }
      if (env.t === 'error' && (env.code === 'pair_invalid' || env.code === 'room_not_found')) {
        // Nobody listening: Devdy is offline, or no master password is set.
        finish(new LoginError('host_offline'))
        return
      }
      if (env.t === 'peer_left') {
        finish(new LoginError(session ? 'wrong_password' : 'host_offline'))
        return
      }
      if (!roomId || env.room_id !== roomId || !env.cipher) return
      if (env.t === 'control' && !session) {
        let offer
        try {
          offer = parseHsOffer(JSON.parse(base64ToUtf8(env.cipher)))
        } catch {
          return
        }
        if (!offer) return
        const keypair = generateKeypair()
        send(controlFrame(roomId, utf8ToBase64(hsAnswerJson(toHex(keypair.publicKey))), ++seq))
        session = deriveSessionKey(fromHex(offer.host_pub), psk, keypair).session
        // Any sealed frame proves the password; its content is not used.
        const proof = new TextEncoder().encode(JSON.stringify({ kind: 'cmd', action: 'login' }))
        send(cmdFrame(roomId, session.seal(proof), ++seq))
        return
      }
      if (env.t === 'stream' && session) {
        let payload: Record<string, unknown>
        try {
          payload = JSON.parse(new TextDecoder().decode(session.open(env.cipher)))
        } catch {
          return
        }
        if (payload.kind !== 'login_granted') return
        const str = (k: string): string => (typeof payload[k] === 'string' ? (payload[k] as string) : '')
        finish(null, {
          link: {
            relay_url: relayUrl,
            host_fingerprint: str('host_fingerprint'),
            rendezvous_code: str('rendezvous_code'),
            run_id: str('run_id'),
            link_secret: str('link_secret'),
          },
          token: str('token'),
        })
      }
    }
  })
}
