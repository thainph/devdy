// Mask anything in a short display text (a tool's command / target) that may be
// a credential, before it is shown on a dashboard card or log line. Mirrors
// `sanitize` in `src-tauri/src/runs/runtime.rs` (the backend's `currentTool`
// summaries), plus a few command-line forms: `-u user:pass`, `-p<pass>` /
// `-p pass` in sshpass / mysql commands, and JSON `"…token…": "…"` pairs.
// The permission prompt keeps the full command: the user must see what runs.
//
// Pure logic (no Vue / Tauri / `@/` imports) for node:test.

export const MASK = '***'

/** Prefixes of well-known credential formats. */
const TOKEN_PREFIXES = [
  'ghp_', 'gho_', 'ghu_', 'ghs_', 'ghr_', 'github_pat_', 'glpat-', 'gldt-', 'sk-', 'sk_live_', 'rk_live_',
  'xoxb-', 'xoxp-', 'xoxa-', 'xoxs-', 'xapp-', 'AKIA', 'ASIA', 'eyJ', 'ya29.', 'AIza',
]

/** Header names / words after which the next word is a credential. */
const SECRET_LEADERS = ['authorization:', 'bearer', 'basic', 'x-api-key:', 'private-token:', 'cookie:', 'token', 'apikey:']

/** Key words that make a `key=value` / `--key value` a secret. */
const SECRET_KEYS = [
  'token', 'secret', 'pass', 'pwd', 'auth', 'cred', 'cookie', 'apikey', 'api_key', 'api-key', 'private', 'session', 'key',
]

/** Commands whose `-p` carries a password (`sshpass -p x`, `mysql -px`). */
const PASSWORD_P_COMMANDS = new Set(['sshpass', 'mysql', 'mysqldump', 'mysqladmin', 'mariadb', 'mariadb-dump'])

/** JSON string pairs whose key names a secret: `"password": "…"`. */
const JSON_SECRET_RE = /("[^"]*(?:pass|token|secret)[^"]*"\s*:\s*")[^"]*(")/gi

function isSecretKey(key: string): boolean {
  const k = key.replace(/^-+/, '').toLowerCase()
  return SECRET_KEYS.some((w) => k.includes(w))
}

/** An env-var style name (`FOO_BAR`, `GH_TOKEN`). */
function isEnvName(key: string): boolean {
  return /^[A-Z_][A-Z0-9_]*$/.test(key)
}

/** A word without the quotes / brackets around it. */
function core(word: string): string {
  return word.replace(/^["'`()[\]{},;]+|["'`()[\]{},;]+$/g, '')
}

function looksLikeToken(word: string): boolean {
  const w = core(word)
  if (TOKEN_PREFIXES.some((p) => w.startsWith(p) && w.length >= p.length + 8)) return true
  // Long opaque strings (keys, hashes): letters and digits, no path parts.
  return w.length >= 32 && /^[A-Za-z0-9_\-+=]+$/.test(w) && /\d/.test(w) && /[A-Za-z]/.test(w)
}

/** A URL without credentials, query string or fragment. */
function stripUrl(url: string): string {
  const i = url.indexOf('://')
  if (i < 0) return url
  const scheme = url.slice(0, i + 3)
  const rest = url.slice(i + 3).split(/[?#]/)[0]
  const slash = rest.indexOf('/')
  const authority = slash < 0 ? rest : rest.slice(0, slash)
  const path = slash < 0 ? '' : rest.slice(slash)
  const host = authority.slice(authority.lastIndexOf('@') + 1)
  return `${scheme}${host}${path}`
}

/** `user:pass` → `user:***`. */
function maskUserPass(word: string): string {
  const i = word.indexOf(':')
  return i < 0 ? word : `${word.slice(0, i + 1)}${MASK}`
}

/**
 * Collapse whitespace and mask anything that may be a credential: values of
 * env assignments and secret-named flags, the word after an auth header,
 * token-looking strings, URL credentials / query strings, `-u user:pass`,
 * sshpass / mysql passwords and secret-named JSON values.
 */
export function sanitizeSecrets(text: string): string {
  const words = text.replace(JSON_SECRET_RE, `$1${MASK}$2`).split(/\s+/).filter(Boolean)
  const passwordP = words.some((w) => PASSWORD_P_COMMANDS.has(core(w).split('/').pop() ?? ''))
  const out: string[] = []
  let maskNext = false
  let userNext = false
  for (const word of words) {
    const c = core(word)
    const lower = c.toLowerCase()
    const leader = SECRET_LEADERS.some((l) => lower === l || lower.endsWith(`-${l}`))
    if (userNext) {
      userNext = false
      out.push(maskUserPass(word))
      continue
    }
    if (maskNext) {
      // `Authorization: Bearer <token>`: the scheme word is kept, the credential masked.
      if (leader) {
        out.push(word)
        continue
      }
      maskNext = false
      out.push(MASK)
      continue
    }
    if (leader) {
      out.push(word)
      maskNext = true
      continue
    }
    // `curl -u user:pass`
    if (c === '-u' || c === '--user') {
      out.push(word)
      userNext = true
      continue
    }
    if (passwordP && c === '-p') {
      out.push(word)
      maskNext = true
      continue
    }
    if (passwordP && /^-p.+/.test(c)) {
      out.push(`-p${MASK}`)
      continue
    }
    // `--token value` / `--password value`.
    if (c.startsWith('--') && !c.includes('=') && isSecretKey(c)) {
      out.push(word)
      maskNext = true
      continue
    }
    if (c.includes('://')) {
      out.push(stripUrl(c))
      continue
    }
    const eq = c.indexOf('=')
    if (eq > 0) {
      const key = c.slice(0, eq)
      const value = c.slice(eq + 1)
      const bare = key.replace(/^-+/, '')
      if (bare && /^[A-Za-z0-9_.-]+$/.test(bare) && value && (isEnvName(bare) || isSecretKey(bare) || looksLikeToken(value))) {
        out.push(`${key}=${MASK}`)
        continue
      }
    }
    out.push(looksLikeToken(word) ? MASK : word)
  }
  return out.join(' ')
}
