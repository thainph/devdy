// Todo source normalisation (docs/focus-board-spec.md BR-02).
//
// Mirror of src-tauri/src/focus/source.rs (and the Slack link parser in
// src-tauri/src/slack_threads/link.rs): both sides run the shared vectors in
// src-tauri/src/focus/source_vectors.json, so keep them in lockstep.
//
//   Slack thread link (a reply → its root)        → slack  `slack:<team>/<channel>/<ts>`
//   github.com/<o>/<r>/(issues|pull)/<n> | <o>/<r>#<n> → github `github:<o>/<r>#<n>` (lowercase)
//   <host>/<path>/-/(issues|merge_requests)/<n>    → gitlab `gitlab:<path>#<n>` / `!<n>`
//   run:<id> | devdy://run/<id>                    → run    `run:<id>`
//   any other http(s) URL                          → url    (no ref)
//
// The URL is kept as given (trimmed, `<…>` unwrapped); anything else throws.

export const INVALID_SOURCE_MESSAGE =
  'invalid source: expected a link (Slack thread, GitHub/GitLab issue or PR, run:<id>, or an http(s) URL)';

// ---- Slack (port of slack_threads/link.rs) ----------------------------------------

function isSlackHost(host) {
  if (!host.endsWith('.slack.com')) return false;
  const rest = host.slice(0, -'.slack.com'.length);
  const label = rest.endsWith('.enterprise') ? rest.slice(0, -'.enterprise'.length) : rest;
  return /^[a-z0-9][a-z0-9-]*$/.test(label);
}

const isSlackTs = (ts) => /^[0-9]{10}\.[0-9]{6}$/.test(ts);

/** Minimal `%XX` decoding for query values (invalid escapes kept verbatim, `+` → space). */
function percentDecode(s) {
  const bytes = Buffer.from(s, 'utf8');
  const out = [];
  let i = 0;
  while (i < bytes.length) {
    if (bytes[i] === 0x25 && i + 2 < bytes.length) {
      const hex = bytes.subarray(i + 1, i + 3).toString('latin1');
      if (/^[0-9A-Fa-f]{2}$/.test(hex)) {
        out.push(parseInt(hex, 16));
        i += 3;
        continue;
      }
    }
    out.push(bytes[i] === 0x2b ? 0x20 : bytes[i]);
    i += 1;
  }
  return Buffer.from(out).toString('utf8');
}

const splitOnce = (s, sep) => {
  const i = s.indexOf(sep);
  return i < 0 ? null : [s.slice(0, i), s.slice(i + sep.length)];
};

/** `{ host, channelId, threadTs, canonicalUrl }`, or throws 'invalid' / 'dm'. */
export function parseSlackThreadLink(input) {
  let s = String(input ?? '').trim();
  if (s.startsWith('<') && s.endsWith('>')) s = s.slice(1, -1).trim();
  const schemeSplit = splitOnce(s, '://');
  if (!schemeSplit) throw new Error('invalid');
  const [scheme, afterScheme] = schemeSplit;
  if (!['https', 'http'].includes(scheme.toLowerCase())) throw new Error('invalid');
  const rest = afterScheme.split('#')[0];
  const authEnd = rest.search(/[/?]/);
  const end = authEnd < 0 ? rest.length : authEnd;
  const host = rest.slice(0, end).toLowerCase();
  if (!isSlackHost(host)) throw new Error('invalid');
  const tail = rest.slice(end);
  const [rawPath, query] = splitOnce(tail, '?') ?? [tail, ''];

  const path = rawPath.endsWith('/') ? rawPath.slice(0, -1) : rawPath;
  if (!path.startsWith('/archives/')) throw new Error('invalid');
  const parts = splitOnce(path.slice('/archives/'.length), '/');
  if (!parts) throw new Error('invalid');
  const [channel, p] = parts;
  if (!p.startsWith('p')) throw new Error('invalid');
  const digits = p.slice(1);
  const channelOk = channel.length >= 9 && 'CGD'.includes(channel[0]) && /^[A-Z0-9]+$/.test(channel);
  if (!channelOk || !/^[0-9]{16}$/.test(digits)) throw new Error('invalid');
  if (channel.startsWith('D')) throw new Error('dm');
  const messageTs = `${digits.slice(0, 10)}.${digits.slice(10)}`;

  let threadTs = null;
  for (const pair of query.split('&').filter(Boolean)) {
    const [k, raw] = splitOnce(pair, '=') ?? [pair, ''];
    const v = percentDecode(raw);
    if (k === 'thread_ts' && isSlackTs(v)) threadTs = v;
    else if (k === 'cid' && v !== channel) throw new Error('invalid');
  }
  threadTs ??= messageTs;
  return {
    host,
    channelId: channel,
    threadTs,
    canonicalUrl: `https://${host}/archives/${channel}/p${threadTs.replace('.', '')}`,
  };
}

// ---- Normalise -----------------------------------------------------------------------

const isOwner = (s) => s.length > 0 && s.length <= 39 && !s.startsWith('-') && /^[A-Za-z0-9-]+$/.test(s);
const isRepo = (s) => s.length > 0 && s.length <= 100 && s !== '.' && s !== '..' && /^[A-Za-z0-9._-]+$/.test(s);
const isRunId = (s) => s.length > 0 && s.length <= 64 && /^[A-Za-z0-9_-]+$/.test(s);
const number = (s) => {
  if (!/^[0-9]{1,10}$/.test(s ?? '')) return null;
  const n = Number(s);
  return n > 0 ? n : null;
};
const githubRef = (owner, repo, n) => `github:${owner.toLowerCase()}/${repo.toLowerCase()}#${n}`;

/** `[host, segments]` of an http(s) URL (query / fragment dropped), or null. */
function httpParts(s) {
  const sp = splitOnce(s, '://');
  if (!sp || !['https', 'http'].includes(sp[0].toLowerCase())) return null;
  const rest = sp[1].split(/[?#]/)[0];
  const [host, path] = splitOnce(rest, '/') ?? [rest, ''];
  if (!host || /\s/.test(host)) return null;
  return [host.toLowerCase(), path.split('/').filter(Boolean)];
}

function githubFromUrl(host, segs) {
  if (host !== 'github.com' && host !== 'www.github.com') return null;
  const [owner, repo, kind, n] = segs;
  if (segs.length < 4 || !['issues', 'pull'].includes(kind) || !isOwner(owner) || !isRepo(repo)) return null;
  const num = number(n);
  return num ? githubRef(owner, repo, num) : null;
}

function gitlabFromUrl(segs) {
  const dash = segs.indexOf('-');
  if (dash <= 0) return null;
  const sep = { issues: '#', merge_requests: '!' }[segs[dash + 1]];
  const n = number(segs[dash + 2]);
  if (!sep || !n) return null;
  return `gitlab:${segs.slice(0, dash).join('/').toLowerCase()}${sep}${n}`;
}

/**
 * Normalise a todo's source link → `{ source_type, source_ref, source_url }`,
 * or null for an empty input; throws Error(INVALID_SOURCE_MESSAGE) otherwise.
 */
export function normalizeSource(input) {
  let s = String(input ?? '').trim();
  if (s.startsWith('<') && s.endsWith('>')) s = s.slice(1, -1).trim();
  if (!s) return null;
  const found = (source_type, source_ref = null) => ({ source_type, source_ref, source_url: s });

  for (const prefix of ['run:', 'devdy://run/', 'devdy://runs/']) {
    if (s.startsWith(prefix)) {
      const id = s.slice(prefix.length).replace(/\/+$/, '');
      if (!isRunId(id)) throw new Error(INVALID_SOURCE_MESSAGE);
      return found('run', `run:${id}`);
    }
  }
  try {
    const l = parseSlackThreadLink(s);
    const team = l.host.endsWith('.slack.com') ? l.host.slice(0, -'.slack.com'.length) : l.host;
    return found('slack', `slack:${team}/${l.channelId}/${l.threadTs}`);
  } catch {
    // Not a Slack thread (a DM link stays a plain URL).
  }
  const parts = httpParts(s);
  if (parts) {
    const ref = githubFromUrl(...parts);
    if (ref) return found('github', ref);
    const gl = gitlabFromUrl(parts[1]);
    if (gl) return found('gitlab', gl);
    return found('url');
  }
  const hash = s.lastIndexOf('#');
  if (hash >= 0) {
    const or = splitOnce(s.slice(0, hash), '/');
    const n = number(s.slice(hash + 1));
    if (or && n && isOwner(or[0]) && isRepo(or[1])) return found('github', githubRef(or[0], or[1], n));
  }
  throw new Error(INVALID_SOURCE_MESSAGE);
}
