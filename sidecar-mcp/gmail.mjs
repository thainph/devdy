#!/usr/bin/env node
// Devdy built-in MCP server: Gmail (stdio, JSON-RPC 2.0).
//
// Injected under the key `gmail` (tools namespaced `mcp__gmail__*`) when a
// Google account is connected in Devdy Settings. OAuth creds arrive via env and
// are refreshed on demand (see lib/google.mjs). Zero runtime dependencies.
//
// Scope: gmail.modify (read / attachments / send / label / trash — NOT permanent mailbox
// wipe). `delete` needs the broader scope and will fail cleanly if not granted.
// Destructive/outbound tools still hit Devdy's per-tool permission prompt.

import { mkdir, writeFile } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
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

/** Walk a payload tree and collect every attachment-ish part (real attachments
 *  plus inline images such as signature logos, which carry a Content-ID). */
function collectAttachments(payload, out = []) {
  if (!payload) return out;
  const body = payload.body || {};
  if (payload.filename || body.attachmentId) {
    const disposition = headerOf(payload, 'Content-Disposition');
    out.push({
      filename: payload.filename || `part-${payload.partId || out.length + 1}`,
      mimeType: payload.mimeType || 'application/octet-stream',
      size: Number(body.size) || 0,
      attachmentId: body.attachmentId || '',
      data: body.data || '',
      inline: /inline/i.test(disposition),
      contentId: headerOf(payload, 'Content-ID').replace(/^<|>$/g, ''),
    });
  }
  for (const part of payload.parts || []) collectAttachments(part, out);
  return out;
}

/** Fetch an attachment's bytes: small parts inline the data, big ones need a
 *  second call against the attachments endpoint. */
async function attachmentBuffer(messageId, att) {
  if (att.data) return Buffer.from(att.data, 'base64url');
  if (!att.attachmentId) throw new Error(`Attachment '${att.filename}' has no downloadable body.`);
  const r = await gapi(`${BASE}/messages/${messageId}/attachments/${att.attachmentId}`);
  return Buffer.from(r.data || '', 'base64url');
}

/** Strip any directory component / unsafe chars so a mail-supplied filename can
 *  never escape the destination directory. */
function safeName(name) {
  return basename(String(name || '')).replace(/[/\\<>:"|?*\x00-\x1f]/g, '_') || 'attachment';
}

const fmtSize = (n) => (n >= 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)}MB` : n >= 1024 ? `${Math.round(n / 1024)}KB` : `${n}B`);

const fmtAttachment = (a, i) =>
  `- [${i + 1}] ${a.filename}  ${a.mimeType} · ${fmtSize(a.size)}${a.inline ? ' · inline' : ''}${a.contentId ? ` · cid:${a.contentId}` : ''}\n  attachment_id: ${a.attachmentId || '(inline body, no id)'}`;

/** Load a message and its attachment list, applying the shared filters. */
async function loadAttachments(a) {
  const m = await gapi(`${BASE}/messages/${a.message_id}?format=full`);
  let atts = collectAttachments(m.payload);
  if (a.skip_inline) atts = atts.filter((x) => !x.inline);
  if (a.mime_prefix) atts = atts.filter((x) => x.mimeType.startsWith(a.mime_prefix));
  if (a.filename_contains) {
    const needle = a.filename_contains.toLowerCase();
    atts = atts.filter((x) => x.filename.toLowerCase().includes(needle));
  }
  return atts;
}

/** Pick a destination that does not clobber an existing file in the same run. */
function uniquePath(dir, name, used) {
  const dot = name.lastIndexOf('.');
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : '';
  let candidate = name;
  for (let i = 1; used.has(candidate); i += 1) candidate = `${stem}-${i}${ext}`;
  used.add(candidate);
  return join(dir, candidate);
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
      const atts = collectAttachments(m.payload);
      return [
        `From: ${headerOf(m.payload, 'From')}`,
        `To: ${headerOf(m.payload, 'To')}`,
        `Date: ${headerOf(m.payload, 'Date')}`,
        `Subject: ${headerOf(m.payload, 'Subject')}`,
        `Labels: ${(m.labelIds || []).join(', ')}`,
        `Thread: ${m.threadId}`,
        ...(atts.length
          ? [`Attachments (${atts.length}): ${atts.map((x) => `${x.filename} (${fmtSize(x.size)})`).join(', ')} — use list_attachments / download_attachments to save them.`]
          : []),
        '',
        body || '(no plain-text body)',
      ].join('\n');
    },
  },

  list_attachments: {
    description: 'List the attachments and inline images of a message: filename, mimeType, size and the attachment_id needed to download one.',
    inputSchema: {
      type: 'object',
      properties: {
        message_id: { type: 'string' },
        skip_inline: { type: 'boolean', description: 'Hide inline images (signature logos and the like)' },
        mime_prefix: { type: 'string', description: 'Only parts whose mimeType starts with this, e.g. "image/" or "application/pdf"' },
      },
      required: ['message_id'],
    },
    handler: async (a) => {
      const atts = await loadAttachments(a);
      return atts.length ? atts.map(fmtAttachment).join('\n') : 'No attachments.';
    },
  },

  download_attachment: {
    description: 'Download one attachment (image, PDF, any file) from a message to a local path. Identify it by attachment_id, or by filename, or omit both when the message has exactly one attachment.',
    inputSchema: {
      type: 'object',
      properties: {
        message_id: { type: 'string' },
        dest_path: { type: 'string', description: 'Absolute local path to write' },
        attachment_id: { type: 'string', description: 'From list_attachments; most precise' },
        filename_contains: { type: 'string', description: 'Case-insensitive substring of the attachment filename' },
      },
      required: ['message_id', 'dest_path'],
    },
    handler: async (a) => {
      const all = collectAttachments((await gapi(`${BASE}/messages/${a.message_id}?format=full`)).payload);
      let matches = all;
      if (a.attachment_id) {
        matches = all.filter((x) => x.attachmentId === a.attachment_id);
      } else if (a.filename_contains) {
        const needle = a.filename_contains.toLowerCase();
        matches = all.filter((x) => x.filename.toLowerCase().includes(needle));
      }
      if (!matches.length) {
        return all.length
          ? `No attachment matched. This message has:\n${all.map(fmtAttachment).join('\n')}`
          : 'This message has no attachments.';
      }
      if (matches.length > 1) {
        return `Ambiguous: ${matches.length} attachments matched. Pass attachment_id for one of:\n${matches.map(fmtAttachment).join('\n')}`;
      }
      const att = matches[0];
      const buf = await attachmentBuffer(a.message_id, att);
      await mkdir(dirname(a.dest_path), { recursive: true });
      await writeFile(a.dest_path, buf);
      return `Downloaded ${att.filename} (${att.mimeType}, ${buf.length} bytes) → ${a.dest_path}`;
    },
  },

  download_attachments: {
    description: 'Download every attachment of a message into a local directory, keeping the original filenames. Filter with mime_prefix (e.g. "image/") or filename_contains, and skip_inline to ignore signature logos.',
    inputSchema: {
      type: 'object',
      properties: {
        message_id: { type: 'string' },
        dest_dir: { type: 'string', description: 'Absolute local directory; created if missing' },
        mime_prefix: { type: 'string', description: 'Only parts whose mimeType starts with this, e.g. "image/"' },
        filename_contains: { type: 'string' },
        skip_inline: { type: 'boolean', description: 'Skip inline images (signature logos and the like)' },
      },
      required: ['message_id', 'dest_dir'],
    },
    handler: async (a) => {
      const atts = await loadAttachments(a);
      if (!atts.length) return 'No attachments matched — nothing downloaded.';
      await mkdir(a.dest_dir, { recursive: true });
      const used = new Set();
      const lines = [];
      for (const att of atts) {
        const dest = uniquePath(a.dest_dir, safeName(att.filename), used);
        // Sequential: a mail with many parts would otherwise burst the Gmail API.
        const buf = await attachmentBuffer(a.message_id, att);
        await writeFile(dest, buf);
        lines.push(`- ${att.mimeType} · ${buf.length} bytes → ${dest}`);
      }
      return `Downloaded ${lines.length} attachment(s) to ${a.dest_dir}:\n${lines.join('\n')}`;
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
