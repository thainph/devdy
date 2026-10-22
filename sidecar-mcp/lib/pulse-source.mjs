// Source normalisation for Project Pulse (docs/project-pulse-spec.md BR-02).
//
// Mirror of src-tauri/src/pulse/source.rs (and the Slack link parser in
// src-tauri/src/slack_threads/link.rs): both sides run the shared vectors in
// src-tauri/src/pulse/source_vectors.json, so keep them in lockstep.
//
//   Slack thread link            → slack        `<channel_id>:<thread_ts>`
//   github.com/<o>/<r>/issues/<n> | <o>/<r>#<n> → github_issue `<o>/<r>#<n>` (lowercase)
//   bare #<n>                    → needs exactly one GitHub repo in the project

export const INVALID_SOURCE_MESSAGE = 'only Slack thread or GitHub issue links are supported';
const DM_UNSUPPORTED_MESSAGE = 'direct-message threads are not supported';

/** Error carried to the AI as "<code>: <detail>". */
export class PulseError extends Error {
  constructor(code, detail) {
    super(`${code}: ${detail}`);
    this.code = code;
    this.detail = detail;
  }
}

const invalid = (detail = INVALID_SOURCE_MESSAGE) => new PulseError('invalid_source', detail);
const repoAmbiguous = () => new PulseError('repo_ambiguous', 'write the issue as owner/repo#number');

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

// ---- GitHub ------------------------------------------------------------------------

const isOwner = (s) => s.length > 0 && s.length <= 39 && !s.startsWith('-') && /^[A-Za-z0-9-]+$/.test(s);
const isRepo = (s) => s.length > 0 && s.length <= 100 && s !== '.' && s !== '..' && /^[A-Za-z0-9._-]+$/.test(s);
const issueNumber = (s) => {
  if (!/^[0-9]{1,10}$/.test(s)) return null;
  const n = Number(s);
  return n > 0 ? n : null;
};

function githubSource(owner, repo, n) {
  const o = owner.toLowerCase();
  const r = repo.toLowerCase();
  return { source_type: 'github_issue', source_ref: `${o}/${r}#${n}`, source_url: `https://github.com/${o}/${r}/issues/${n}` };
}

/** Source, a thrown PulseError, or null when the input isn't a github.com link. */
function parseGithubUrl(s) {
  let rest = s;
  const schemeSplit = splitOnce(s, '://');
  if (schemeSplit) {
    if (!['https', 'http'].includes(schemeSplit[0].toLowerCase())) return null;
    rest = schemeSplit[1];
  }
  rest = rest.split(/[?#]/)[0];
  const hp = splitOnce(rest, '/');
  if (!hp) return null;
  const host = hp[0].toLowerCase();
  if (host !== 'github.com' && host !== 'www.github.com') return null;
  const path = hp[1].endsWith('/') ? hp[1].slice(0, -1) : hp[1];
  const parts = path.split('/');
  if (parts.length !== 4) throw invalid();
  const [owner, repo, kind, number] = parts;
  if (kind === 'pull') throw invalid('pull requests are not supported (use the PR inbox); link a GitHub issue');
  const n = issueNumber(number);
  if (n && kind === 'issues' && isOwner(owner) && isRepo(repo)) return githubSource(owner, repo, n);
  throw invalid();
}

function parseGithubShort(s, projectRepos) {
  const hash = s.lastIndexOf('#');
  if (hash < 0) return null;
  const head = s.slice(0, hash);
  const n = issueNumber(s.slice(hash + 1));
  if (!n) return null;
  if (!head) {
    const repos = [...new Set(projectRepos.map((r) => String(r).trim().toLowerCase()).filter(Boolean))];
    if (repos.length !== 1) throw repoAmbiguous();
    const or = splitOnce(repos[0], '/');
    if (or && isOwner(or[0]) && isRepo(or[1])) return githubSource(or[0], or[1], n);
    throw repoAmbiguous();
  }
  const or = splitOnce(head, '/');
  if (!or || !isOwner(or[0]) || !isRepo(or[1])) return null;
  return githubSource(or[0], or[1], n);
}

/**
 * Normalise a pasted link / reference → `{ source_type, source_ref, source_url }`;
 * throws PulseError `invalid_source` / `repo_ambiguous`. `projectRepos`
 * (`owner/repo`) only matters for a bare `#<n>`.
 */
export function parseSource(input, projectRepos = []) {
  let s = String(input ?? '').trim();
  if (s.startsWith('<') && s.endsWith('>')) s = s.slice(1, -1).trim();
  if (!s) throw invalid();
  try {
    const l = parseSlackThreadLink(s);
    return { source_type: 'slack', source_ref: `${l.channelId}:${l.threadTs}`, source_url: l.canonicalUrl };
  } catch (e) {
    if (e.message === 'dm') throw invalid(DM_UNSUPPORTED_MESSAGE);
  }
  const gh = parseGithubUrl(s) ?? parseGithubShort(s, projectRepos);
  if (gh) return gh;
  throw invalid();
}
