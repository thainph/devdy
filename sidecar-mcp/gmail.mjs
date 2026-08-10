#!/usr/bin/env node
// Devdy built-in MCP server: Gmail (stdio, JSON-RPC 2.0).
//
// Injected under the key `gmail` (tools namespaced `mcp__gmail__*`) when a
// Google account is connected in Devdy Settings. OAuth creds arrive via env and
// are refreshed on demand (see lib/google.mjs). Zero runtime dependencies.
//
// Scope: gmail.modify (read / send / label / trash — NOT permanent mailbox
// wipe). `delete` needs the broader scope and will fail cleanly if not granted.
// Destructive/outbound tools still hit Devdy's per-tool permission prompt.

import { gapi, runServer } from './lib/google.mjs';

const BASE = 'https://gmail.googleapis.com/gmail/v1/users/me';
const enc = encodeURIComponent;
const b64url = (buf) => Buffer.from(buf).toString('base64url');

/** RFC 2047-encode a header value only when it has non-ASCII chars. */
function encodeHeader(value) {
  return /^[\x00-\x7F]*$/.test(value) ? value : `=?UTF-8?B?${Buffer.from(value, 'utf8').toString('base64')}?=`;
}

/** Build a raw RFC 822 message and base64url-encode it for the Gmail API. */
function buildRaw({ to, cc, bcc, subject, body, headers = {} }) {
  const lines = [];
  if (to) lines.push(`To: ${to}`);
  if (cc) lines.push(`Cc: ${cc}`);
  if (bcc) lines.push(`Bcc: ${bcc}`);
  lines.push(`Subject: ${encodeHeader(subject || '')}`);
  for (const [k, v] of Object.entries(headers)) if (v) lines.push(`${k}: ${v}`);
  lines.push('MIME-Version: 1.0');
  lines.push('Content-Type: text/plain; charset="UTF-8"');
  lines.push('Content-Transfer-Encoding: 8bit');
  lines.push('');
  lines.push(body || '');
  return b64url(lines.join('\r\n'));
}

const headerOf = (payload, name) => {
  const h = ((payload && payload.headers) || []).find((x) => x.name.toLowerCase() === name.toLowerCase());
  return h ? h.value : '';
};

/** Recursively pull the first text/plain (fallback text/html) body out of a payload. */
function extractBody(payload) {
  if (!payload) return '';
  if (payload.body && payload.body.data && (payload.mimeType || '').startsWith('text/')) {
    return Buffer.from(payload.body.data, 'base64url').toString('utf8');
  }
  for (const part of payload.parts || []) {
    if ((part.mimeType || '') === 'text/plain' && part.body && part.body.data) {
      return Buffer.from(part.body.data, 'base64url').toString('utf8');
    }
  }
  for (const part of payload.parts || []) {
    const nested = extractBody(part);
    if (nested) return nested;
  }
  return '';
}

async function fetchSummary(id) {
  const m = await gapi(`${BASE}/messages/${id}?format=metadata&metadataHeaders=Subject&metadataHeaders=From&metadataHeaders=Date`);
  return `- [${id}] ${headerOf(m.payload, 'Date')} · ${headerOf(m.payload, 'From')}\n  ${headerOf(m.payload, 'Subject') || '(no subject)'}${m.snippet ? `\n  ${m.snippet}` : ''}`;
}

const tools = {
  list_messages: {
    description: 'List recent Gmail messages (most recent first). Optionally filter by label ids.',
    inputSchema: {
      type: 'object',
      properties: {
        label_ids: { type: 'array', items: { type: 'string' }, description: 'e.g. ["INBOX"], ["UNREAD"]' },
        limit: { type: 'integer', minimum: 1, maximum: 25 },
      },
    },
    handler: async (a) => {
      const limit = Math.min(Math.max(Number(a.limit) || 10, 1), 25);
      const labels = (a.label_ids || []).map((l) => `labelIds=${enc(l)}`).join('&');
      const data = await gapi(`${BASE}/messages?maxResults=${limit}${labels ? `&${labels}` : ''}`);
      const ids = (data.messages || []).map((m) => m.id);
      if (!ids.length) return 'No messages.';
      return (await Promise.all(ids.map(fetchSummary))).join('\n');
    },
  },

  search: {
    description: 'Search Gmail using the standard query syntax (e.g. "from:alice is:unread subject:invoice after:2024/01/01").',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string' },
        limit: { type: 'integer', minimum: 1, maximum: 25 },
      },
      required: ['query'],
    },
    handler: async (a) => {
      const limit = Math.min(Math.max(Number(a.limit) || 10, 1), 25);
      const data = await gapi(`${BASE}/messages?q=${enc(a.query)}&maxResults=${limit}`);
      const ids = (data.messages || []).map((m) => m.id);
      if (!ids.length) return 'No matching messages.';
      return (await Promise.all(ids.map(fetchSummary))).join('\n');
    },
  },

  read_message: {
    description: 'Read one message in full: headers plus decoded plain-text body.',
    inputSchema: {
      type: 'object',
      properties: { message_id: { type: 'string' } },
      required: ['message_id'],
    },
    handler: async (a) => {
      const m = await gapi(`${BASE}/messages/${a.message_id}?format=full`);
      const body = extractBody(m.payload);
      return [
        `From: ${headerOf(m.payload, 'From')}`,
        `To: ${headerOf(m.payload, 'To')}`,
        `Date: ${headerOf(m.payload, 'Date')}`,
        `Subject: ${headerOf(m.payload, 'Subject')}`,
        `Labels: ${(m.labelIds || []).join(', ')}`,
        `Thread: ${m.threadId}`,
        '',
        body || '(no plain-text body)',
      ].join('\n');
    },
  },

  read_thread: {
    description: 'Read a whole conversation thread: each message summarized with its body.',
    inputSchema: {
      type: 'object',
      properties: { thread_id: { type: 'string' } },
      required: ['thread_id'],
    },
    handler: async (a) => {
      const t = await gapi(`${BASE}/threads/${a.thread_id}?format=full`);
      return (t.messages || [])
        .map((m) => {
          const body = extractBody(m.payload);
          return `── ${headerOf(m.payload, 'From')} · ${headerOf(m.payload, 'Date')}\nSubject: ${headerOf(m.payload, 'Subject')}\n\n${body || '(no plain-text body)'}`;
        })
        .join('\n\n');
    },
  },

  list_labels: {
    description: 'List Gmail labels (id + name), for use with list_messages / modify_labels.',
    inputSchema: { type: 'object', properties: {} },
    handler: async () => {
      const data = await gapi(`${BASE}/labels`);
      const labels = data.labels || [];
      return labels.map((l) => `- ${l.name} [${l.id}]`).join('\n') || 'No labels.';
    },
  },

  send: {
    description: 'Send a new plain-text email.',
    inputSchema: {
      type: 'object',
      properties: {
        to: { type: 'string', description: 'Comma-separated recipients' },
        subject: { type: 'string' },
        body: { type: 'string' },
        cc: { type: 'string' },
        bcc: { type: 'string' },
      },
      required: ['to', 'body'],
    },
    handler: async (a) => {
      const raw = buildRaw(a);
      const r = await gapi(`${BASE}/messages/send`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ raw }),
      });
      return `Sent message ${r.id} (thread ${r.threadId}).`;
    },
  },

  create_draft: {
    description: 'Create a draft email (not sent).',
    inputSchema: {
      type: 'object',
      properties: {
        to: { type: 'string' },
        subject: { type: 'string' },
        body: { type: 'string' },
        cc: { type: 'string' },
        bcc: { type: 'string' },
      },
      required: ['body'],
    },
    handler: async (a) => {
      const raw = buildRaw(a);
      const r = await gapi(`${BASE}/drafts`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ message: { raw } }),
      });
      return `Created draft ${r.id}.`;
    },
  },

  reply: {
    description: 'Reply to a message, staying in its thread. Provide the original message_id and the reply body.',
    inputSchema: {
      type: 'object',
      properties: {
        message_id: { type: 'string' },
        body: { type: 'string' },
        reply_all: { type: 'boolean', description: 'Also Cc the original recipients' },
      },
      required: ['message_id', 'body'],
    },
    handler: async (a) => {
      const orig = await gapi(`${BASE}/messages/${a.message_id}?format=metadata&metadataHeaders=Subject&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Cc&metadataHeaders=Message-ID`);
      const from = headerOf(orig.payload, 'From');
      const msgId = headerOf(orig.payload, 'Message-ID');
      let subject = headerOf(orig.payload, 'Subject') || '';
      if (!/^re:/i.test(subject)) subject = `Re: ${subject}`;
      const cc = a.reply_all ? headerOf(orig.payload, 'Cc') : undefined;
      const raw = buildRaw({
        to: from,
        cc,
        subject,
        body: a.body,
        headers: msgId ? { 'In-Reply-To': msgId, References: msgId } : {},
      });
      const r = await gapi(`${BASE}/messages/send`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ raw, threadId: orig.threadId }),
      });
      return `Replied in thread ${r.threadId} (message ${r.id}).`;
    },
  },

  modify_labels: {
    description: 'Add and/or remove labels on a message (e.g. archive by removing INBOX, or mark read by removing UNREAD).',
    inputSchema: {
      type: 'object',
      properties: {
        message_id: { type: 'string' },
        add_label_ids: { type: 'array', items: { type: 'string' } },
        remove_label_ids: { type: 'array', items: { type: 'string' } },
      },
      required: ['message_id'],
    },
    handler: async (a) => {
      await gapi(`${BASE}/messages/${a.message_id}/modify`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          addLabelIds: a.add_label_ids || [],
          removeLabelIds: a.remove_label_ids || [],
        }),
      });
      return `Updated labels on ${a.message_id}.`;
    },
  },

  mark: {
    description: 'Mark a message read or unread.',
    inputSchema: {
      type: 'object',
      properties: {
        message_id: { type: 'string' },
        read: { type: 'boolean', description: 'true = mark read, false = mark unread' },
      },
      required: ['message_id', 'read'],
    },
    handler: async (a) => {
      const key = a.read ? 'removeLabelIds' : 'addLabelIds';
      await gapi(`${BASE}/messages/${a.message_id}/modify`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ [key]: ['UNREAD'] }),
      });
      return `Marked ${a.message_id} as ${a.read ? 'read' : 'unread'}.`;
    },
  },

  trash: {
    description: 'Move a message to Trash (recoverable for ~30 days).',
    inputSchema: {
      type: 'object',
      properties: { message_id: { type: 'string' } },
      required: ['message_id'],
    },
    handler: async (a) => {
      await gapi(`${BASE}/messages/${a.message_id}/trash`, { method: 'POST' });
      return `Trashed ${a.message_id}.`;
    },
  },

  delete: {
    description: 'PERMANENTLY delete a message (bypasses Trash, cannot be undone). Requires broad Gmail scope; use trash for normal deletion.',
    inputSchema: {
      type: 'object',
      properties: { message_id: { type: 'string' } },
      required: ['message_id'],
    },
    handler: async (a) => {
      await gapi(`${BASE}/messages/${a.message_id}`, { method: 'DELETE', raw: true });
      return `Permanently deleted ${a.message_id}.`;
    },
  },
};

runServer({ serverInfo: { name: 'gmail', version: '0.1.0' }, tools, multiAccount: true });
