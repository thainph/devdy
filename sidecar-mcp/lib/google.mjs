// Shared plumbing for the built-in `google` MCP server (Drive / Gmail / Calendar).
//
// - OAuth: asks Devdy's credential broker (DEVDY_BROKER_SOCK, the socket the
//   gh shim uses) for a short-lived access token per account, so long runs keep
//   working. Refresh tokens and the OAuth client
//   secret never reach this process: its env and command line are readable by
//   any process of the same user, the agent's own shell included.
// - `gapi()`: thin authenticated fetch wrapper over the Google REST APIs.
// - `runServer()`: hand-rolled JSON-RPC 2.0 stdio loop (async handlers), matching
//   the built-in devdy server's protocol. Only JSON frames go to stdout; logs to
//   stderr. Zero runtime dependencies (Node 18+ global fetch).

import { AsyncLocalStorage } from 'node:async_hooks';
import net from 'node:net';

// The broker caches minted tokens itself and may hand out one that is already
// most of an hour old, so only hold it briefly (asking again is a local call).
const TOKEN_TTL_MS = 5 * 60_000;

// Connected accounts, from GOOGLE_ACCOUNTS (JSON array of {label,email,default}).
function parseAccounts() {
  const raw = process.env.GOOGLE_ACCOUNTS;
  if (raw) {
    try {
      const arr = JSON.parse(raw);
      if (Array.isArray(arr) && arr.length) return arr;
    } catch { /* fall through */ }
  }
  return [];
}

const ACCOUNTS = parseAccounts();
// Per-tool-call selected account label (set by runServer). Undefined = default.
export const accountCtx = new AsyncLocalStorage();
// Access-token cache keyed by account label.
const tokenCache = new Map(); // label → { accessToken, expiresAt }

/** Resolve which account to use for the current call. */
function resolveAccount(label) {
  if (!ACCOUNTS.length) {
    throw new Error('No Google account connected. Add one in Devdy Settings → Google Account.');
  }
  if (label) {
    const found = ACCOUNTS.find((a) => a.label === label || a.email === label);
    if (!found) {
      throw new Error(`Unknown account '${label}'. Connected: ${ACCOUNTS.map((a) => a.label).join(', ')}.`);
    }
    return found;
  }
  return ACCOUNTS.find((a) => a.default) || ACCOUNTS[0];
}

/** List connected accounts (for the `list_accounts` tool). */
export function listAccounts() {
  return ACCOUNTS.map((a) => ({ label: a.label, email: a.email, default: !!a.default }));
}

/** One NDJSON request/response over the broker socket. */
function askBroker(req) {
  const sock = process.env.DEVDY_BROKER_SOCK;
  if (!sock || !process.env.DEVDY_PROJECT_ID) {
    return Promise.reject(new Error('Devdy credential broker not configured for this server.'));
  }
  return new Promise((resolve, reject) => {
    const conn = net.createConnection(sock);
    let buf = '';
    let settled = false;
    const done = (fn, arg) => {
      if (settled) return;
      settled = true;
      conn.destroy();
      fn(arg);
    };
    conn.setTimeout(30_000, () => done(reject, new Error('credential broker timed out')));
    conn.on('connect', () => conn.write(`${JSON.stringify(req)}\n`));
    conn.on('data', (d) => {
      buf += d.toString('utf8');
      const nl = buf.indexOf('\n');
      if (nl < 0) return;
      try {
        done(resolve, JSON.parse(buf.slice(0, nl)));
      } catch (e) {
        done(reject, e);
      }
    });
    conn.on('error', () => done(reject, new Error('credential broker unreachable')));
    conn.on('close', () => done(reject, new Error('credential broker closed the connection')));
  });
}

/** Access token for the current call's account; `refresh` skips every cache
 *  (after a 401). */
export async function getAccessToken({ refresh = false } = {}) {
  const account = resolveAccount(accountCtx.getStore());
  const cached = tokenCache.get(account.label);
  if (!refresh && cached && Date.now() < cached.expiresAt) return cached.accessToken;
  const resp = await askBroker({
    project_id: process.env.DEVDY_PROJECT_ID,
    run_id: process.env.DEVDY_RUN_ID || null,
    tool: 'google',
    argv: [],
    host: null,
    account: account.label,
    refresh,
  });
  if (!resp || resp.decision !== 'allow' || !resp.token) {
    throw new Error(`No Google access token for '${account.label}': ${(resp && resp.reason) || 'denied'}`);
  }
  tokenCache.set(account.label, { accessToken: resp.token, expiresAt: Date.now() + TOKEN_TTL_MS });
  return resp.token;
}

/** Authenticated fetch against a Google API. Throws on non-2xx with a trimmed
 *  body so the model sees the actual error. `raw:true` returns the Response
 *  (for streaming/binary/text downloads); otherwise JSON or text is returned. */
export async function gapi(url, opts = {}) {
  const { method = 'GET', headers = {}, body, raw = false } = opts;
  const send = async (refresh) => fetch(url, {
    method,
    headers: { Authorization: `Bearer ${await getAccessToken({ refresh })}`, ...headers },
    body,
  });
  let res = await send(false);
  // A cached token can die early (revoked, or the machine slept past its
  // expiry): retry once with a freshly minted one.
  if (res.status === 401) res = await send(true);
  if (!res.ok) {
    let detail = '';
    try {
      detail = (await res.text()).slice(0, 500);
    } catch {}
    throw new Error(`${method} ${url.split('?')[0]} → ${res.status}: ${detail}`);
  }
  if (raw) return res;
  const ct = res.headers.get('content-type') || '';
  return ct.includes('application/json') ? res.json() : res.text();
}

/** Hand-rolled JSON-RPC 2.0 stdio server. `tools` is a map of
 *  { name: { description, inputSchema, handler(args) -> string|Promise<string> } }.
 *  With `multiAccount: true`, every tool gains an optional `account` argument
 *  (which connected account to act as), a `list_accounts` tool is added, and each
 *  handler runs inside the account context so getAccessToken() picks the right
 *  account automatically. */
export function runServer({ serverInfo, tools, multiAccount = false }) {
  const DEFAULT_PROTOCOL = '2025-06-18';

  if (multiAccount) {
    for (const t of Object.values(tools)) {
      t.inputSchema = t.inputSchema || { type: 'object', properties: {} };
      t.inputSchema.properties = t.inputSchema.properties || {};
      if (!t.inputSchema.properties.account) {
        t.inputSchema.properties.account = {
          type: 'string',
          description: 'Which connected Google account (its label or email). Omit to use the default account.',
        };
      }
    }
    tools.list_accounts = {
      description: 'List the connected Google accounts (label, email, which is default). Use a label as the `account` argument on other tools.',
      inputSchema: { type: 'object', properties: {} },
      handler: () => {
        const rows = listAccounts();
        return rows.length
          ? rows.map((a) => `- ${a.label}${a.email ? ` <${a.email}>` : ''}${a.default ? ' (default)' : ''}`).join('\n')
          : 'No accounts connected.';
      },
    };
  }

  const log = (...a) => process.stderr.write(`[${serverInfo.name}-mcp] ${a.join(' ')}\n`);
  const send = (msg) => process.stdout.write(`${JSON.stringify(msg)}\n`);
  const reply = (id, result) => send({ jsonrpc: '2.0', id, result });
  const replyError = (id, code, message) => send({ jsonrpc: '2.0', id, error: { code, message } });

  async function handle(msg) {
    const { id, method, params } = msg;
    const isRequest = id !== undefined && id !== null;
    switch (method) {
      case 'initialize':
        reply(id, {
          protocolVersion: (params && params.protocolVersion) || DEFAULT_PROTOCOL,
          capabilities: { tools: { listChanged: false } },
          serverInfo,
        });
        return;
      case 'notifications/initialized':
      case 'notifications/cancelled':
        return;
      case 'ping':
        if (isRequest) reply(id, {});
        return;
      case 'tools/list':
        reply(id, {
          tools: Object.entries(tools).map(([name, t]) => ({
            name,
            description: t.description,
            inputSchema: t.inputSchema,
          })),
        });
        return;
      case 'tools/call': {
        const name = params && params.name;
        const tool = tools[name];
        if (!tool) {
          replyError(id, -32602, `Unknown tool: ${name}`);
          return;
        }
        try {
          const args = (params && params.arguments) || {};
          const text = multiAccount
            ? await accountCtx.run(args.account, () => tool.handler(args))
            : await tool.handler(args);
          reply(id, { content: [{ type: 'text', text: String(text) }] });
        } catch (e) {
          reply(id, {
            content: [{ type: 'text', text: `Error: ${e && e.message ? e.message : e}` }],
            isError: true,
          });
        }
        return;
      }
      case 'resources/list':
        reply(id, { resources: [] });
        return;
      case 'prompts/list':
        reply(id, { prompts: [] });
        return;
      default:
        if (isRequest) replyError(id, -32601, `Method not found: ${method}`);
    }
  }

  let buffer = '';
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    buffer += chunk;
    let nl;
    while ((nl = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (!line) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch (e) {
        log('failed to parse line:', e.message);
        continue;
      }
      Promise.resolve(handle(msg)).catch((e) => {
        log('handler crashed:', e && e.stack ? e.stack : e);
        if (msg && msg.id != null) replyError(msg.id, -32603, 'Internal error');
      });
    }
  });
  process.stdin.on('end', () => process.exit(0));
  process.on('SIGTERM', () => process.exit(0));
  process.on('SIGINT', () => process.exit(0));
  log('ready');
}
