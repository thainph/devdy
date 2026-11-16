// One NDJSON request/response over Devdy's broker socket (DEVDY_BROKER_SOCK),
// shared by the built-in servers: Google access tokens (google.mjs) and Delivery
// Radar refreshes (radar-store.mjs). The broker only answers a live run of the
// claimed project, so the request carries DEVDY_PROJECT_ID / DEVDY_RUN_ID.

import net from 'node:net';

export function askBroker(req, { timeoutMs = 30_000 } = {}) {
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
    conn.setTimeout(timeoutMs, () => done(reject, new Error('credential broker timed out')));
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
