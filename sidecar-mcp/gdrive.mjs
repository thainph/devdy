#!/usr/bin/env node
// Devdy built-in MCP server: Google Drive (stdio, JSON-RPC 2.0).
//
// Injected under the key `gdrive` (tools namespaced `mcp__gdrive__*`) when a
// Google account is connected in Devdy Settings. OAuth creds arrive via env and
// are refreshed on demand (see lib/google.mjs). Zero runtime dependencies.
//
// Full CRUD + sharing: list/search/read/download/mkdir/upload/create/update/
// rename/move/delete (permanent — QĐ)/share/permissions. Destructive tools still
// go through Devdy's per-tool permission prompt on the AI side.

import { readFile, writeFile } from 'node:fs/promises';
import { basename } from 'node:path';
import { gapi, runServer } from './lib/google.mjs';

const BASE = 'https://www.googleapis.com/drive/v3';
const UPLOAD = 'https://www.googleapis.com/upload/drive/v3/files';
// supportsAllDrives so shared-drive items work too.
const SHARED = 'supportsAllDrives=true&includeItemsFromAllDrives=true';
const FILE_FIELDS = 'id,name,mimeType,size,modifiedTime,parents,webViewLink';

// Google-native docs must be exported; map to a text-friendly format.
const EXPORT_MAP = {
  'application/vnd.google-apps.document': 'text/markdown',
  'application/vnd.google-apps.spreadsheet': 'text/csv',
  'application/vnd.google-apps.presentation': 'text/plain',
  'application/vnd.google-apps.script': 'application/vnd.google-apps.script+json',
};

const enc = encodeURIComponent;
const fmtFile = (f) =>
  `- ${f.name}  [${f.id}]  ${f.mimeType}${f.size ? ` · ${f.size}B` : ''}${f.modifiedTime ? ` · ${f.modifiedTime}` : ''}`;

/** Multipart/related upload body (metadata + media) as a Buffer. */
function multipartBody(metadata, mediaBuffer, mediaMime, boundary) {
  const head = Buffer.from(
    `--${boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n${JSON.stringify(metadata)}\r\n` +
      `--${boundary}\r\nContent-Type: ${mediaMime}\r\n\r\n`,
  );
  const tail = Buffer.from(`\r\n--${boundary}--\r\n`);
  return Buffer.concat([head, mediaBuffer, tail]);
}

const tools = {
  list: {
    description:
      'List Google Drive files. Optionally scope to a folder via folder_id. Returns name, id, mimeType. Folders have mimeType application/vnd.google-apps.folder.',
    inputSchema: {
      type: 'object',
      properties: {
        folder_id: { type: 'string', description: 'Parent folder id; omit for the Drive root' },
        limit: { type: 'integer', minimum: 1, maximum: 200 },
      },
    },
    handler: async (a) => {
      const limit = Math.min(Math.max(Number(a.limit) || 50, 1), 200);
      const q = a.folder_id ? `'${a.folder_id}' in parents and trashed=false` : 'trashed=false';
      const data = await gapi(
        `${BASE}/files?q=${enc(q)}&pageSize=${limit}&orderBy=folder,modifiedTime desc&fields=files(${FILE_FIELDS})&${SHARED}`,
      );
      const files = data.files || [];
      return files.length ? files.map(fmtFile).join('\n') : 'No files found.';
    },
  },

  search: {
    description:
      'Search Drive by free text (matches file name and full text content). For advanced use pass a raw Drive `q` query instead.',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Free-text search term' },
        q: { type: 'string', description: 'Raw Drive query, e.g. "mimeType=\'application/pdf\'"' },
        limit: { type: 'integer', minimum: 1, maximum: 200 },
      },
    },
    handler: async (a) => {
      const limit = Math.min(Math.max(Number(a.limit) || 25, 1), 200);
      let q = a.q;
      if (!q) {
        if (!a.query) throw new Error('Provide `query` or `q`.');
        const t = String(a.query).replace(/'/g, "\\'");
        q = `(name contains '${t}' or fullText contains '${t}') and trashed=false`;
      }
      const data = await gapi(
        `${BASE}/files?q=${enc(q)}&pageSize=${limit}&fields=files(${FILE_FIELDS})&${SHARED}`,
      );
      const files = data.files || [];
      return files.length ? files.map(fmtFile).join('\n') : 'No matching files.';
    },
  },

  get_metadata: {
    description: 'Get full metadata for one file/folder by id.',
    inputSchema: {
      type: 'object',
      properties: { file_id: { type: 'string' } },
      required: ['file_id'],
    },
    handler: async (a) => {
      const f = await gapi(
        `${BASE}/files/${a.file_id}?fields=${FILE_FIELDS},owners,shared,createdTime,description&${SHARED}`,
      );
      return JSON.stringify(f, null, 2);
    },
  },

  read: {
    description:
      'Read a file\'s text content. Google Docs → Markdown, Sheets → CSV, Slides → text (auto-exported). Binary/non-text files should be downloaded instead.',
    inputSchema: {
      type: 'object',
      properties: { file_id: { type: 'string' } },
      required: ['file_id'],
    },
    handler: async (a) => {
      const meta = await gapi(`${BASE}/files/${a.file_id}?fields=id,name,mimeType&${SHARED}`);
      if (meta.mimeType && meta.mimeType.startsWith('application/vnd.google-apps')) {
        const exportMime = EXPORT_MAP[meta.mimeType] || 'text/plain';
        const res = await gapi(
          `${BASE}/files/${a.file_id}/export?mimeType=${enc(exportMime)}`,
          { raw: true },
        );
        return await res.text();
      }
      const res = await gapi(`${BASE}/files/${a.file_id}?alt=media&${SHARED}`, { raw: true });
      const ct = res.headers.get('content-type') || '';
      if (/^(text\/|application\/(json|xml|javascript|x-yaml))/.test(ct)) return await res.text();
      const buf = Buffer.from(await res.arrayBuffer());
      // Best-effort: return text if it looks like UTF-8, else advise download.
      const text = buf.toString('utf8');
      if (!text.includes('�')) return text;
      return `[binary ${meta.mimeType}, ${buf.length} bytes] Use gdrive download to save it locally.`;
    },
  },

  download: {
    description: 'Download a Drive file to a local path. Google-native docs are exported to a text format.',
    inputSchema: {
      type: 'object',
      properties: {
        file_id: { type: 'string' },
        dest_path: { type: 'string', description: 'Absolute local path to write' },
      },
      required: ['file_id', 'dest_path'],
    },
    handler: async (a) => {
      const meta = await gapi(`${BASE}/files/${a.file_id}?fields=id,name,mimeType&${SHARED}`);
      let res;
      if (meta.mimeType && meta.mimeType.startsWith('application/vnd.google-apps')) {
        const exportMime = EXPORT_MAP[meta.mimeType] || 'text/plain';
        res = await gapi(`${BASE}/files/${a.file_id}/export?mimeType=${enc(exportMime)}`, { raw: true });
      } else {
        res = await gapi(`${BASE}/files/${a.file_id}?alt=media&${SHARED}`, { raw: true });
      }
      const buf = Buffer.from(await res.arrayBuffer());
      await writeFile(a.dest_path, buf);
      return `Downloaded ${meta.name} (${buf.length} bytes) → ${a.dest_path}`;
    },
  },

  mkdir: {
    description: 'Create a folder. Optionally nest it under parent_id.',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string' },
        parent_id: { type: 'string' },
      },
      required: ['name'],
    },
    handler: async (a) => {
      const body = {
        name: a.name,
        mimeType: 'application/vnd.google-apps.folder',
        ...(a.parent_id ? { parents: [a.parent_id] } : {}),
      };
      const f = await gapi(`${BASE}/files?fields=${FILE_FIELDS}&${SHARED}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      return `Created folder ${f.name} [${f.id}].`;
    },
  },

  create: {
    description: 'Create a new text file with the given content on Drive.',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string' },
        content: { type: 'string' },
        mime_type: { type: 'string', description: 'Defaults to text/plain' },
        parent_id: { type: 'string' },
      },
      required: ['name', 'content'],
    },
    handler: async (a) => {
      const mime = a.mime_type || 'text/plain';
      const boundary = `devdy${process.pid}${Date.now()}`;
      const metadata = { name: a.name, ...(a.parent_id ? { parents: [a.parent_id] } : {}) };
      const body = multipartBody(metadata, Buffer.from(a.content, 'utf8'), mime, boundary);
      const f = await gapi(`${UPLOAD}?uploadType=multipart&fields=${FILE_FIELDS}&${SHARED}`, {
        method: 'POST',
        headers: { 'Content-Type': `multipart/related; boundary=${boundary}` },
        body,
      });
      return `Created ${f.name} [${f.id}].`;
    },
  },

  upload: {
    description: 'Upload a local file to Drive. Optionally set name and parent_id.',
    inputSchema: {
      type: 'object',
      properties: {
        local_path: { type: 'string', description: 'Absolute local path to upload' },
        name: { type: 'string', description: 'Defaults to the local filename' },
        mime_type: { type: 'string', description: 'Defaults to application/octet-stream' },
        parent_id: { type: 'string' },
      },
      required: ['local_path'],
    },
    handler: async (a) => {
      const data = await readFile(a.local_path);
      const name = a.name || basename(a.local_path);
      const mime = a.mime_type || 'application/octet-stream';
      const boundary = `devdy${process.pid}${Date.now()}`;
      const metadata = { name, ...(a.parent_id ? { parents: [a.parent_id] } : {}) };
      const body = multipartBody(metadata, data, mime, boundary);
      const f = await gapi(`${UPLOAD}?uploadType=multipart&fields=${FILE_FIELDS}&${SHARED}`, {
        method: 'POST',
        headers: { 'Content-Type': `multipart/related; boundary=${boundary}` },
        body,
      });
      return `Uploaded ${f.name} [${f.id}] (${data.length} bytes).`;
    },
  },

  update: {
    description: 'Overwrite an existing file\'s content with new text.',
    inputSchema: {
      type: 'object',
      properties: {
        file_id: { type: 'string' },
        content: { type: 'string' },
        mime_type: { type: 'string', description: 'Defaults to text/plain' },
      },
      required: ['file_id', 'content'],
    },
    handler: async (a) => {
      const mime = a.mime_type || 'text/plain';
      const f = await gapi(
        `${UPLOAD}/${a.file_id}?uploadType=media&fields=${FILE_FIELDS}&${SHARED}`,
        {
          method: 'PATCH',
          headers: { 'Content-Type': mime },
          body: Buffer.from(a.content, 'utf8'),
        },
      );
      return `Updated ${f.name} [${f.id}].`;
    },
  },

  rename: {
    description: 'Rename a file or folder.',
    inputSchema: {
      type: 'object',
      properties: { file_id: { type: 'string' }, name: { type: 'string' } },
      required: ['file_id', 'name'],
    },
    handler: async (a) => {
      const f = await gapi(`${BASE}/files/${a.file_id}?fields=${FILE_FIELDS}&${SHARED}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: a.name }),
      });
      return `Renamed to ${f.name} [${f.id}].`;
    },
  },

  move: {
    description: 'Move a file/folder into a different parent folder.',
    inputSchema: {
      type: 'object',
      properties: {
        file_id: { type: 'string' },
        new_parent_id: { type: 'string' },
      },
      required: ['file_id', 'new_parent_id'],
    },
    handler: async (a) => {
      const meta = await gapi(`${BASE}/files/${a.file_id}?fields=parents&${SHARED}`);
      const removeParents = (meta.parents || []).join(',');
      const f = await gapi(
        `${BASE}/files/${a.file_id}?addParents=${enc(a.new_parent_id)}${removeParents ? `&removeParents=${enc(removeParents)}` : ''}&fields=${FILE_FIELDS}&${SHARED}`,
        { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: '{}' },
      );
      return `Moved ${f.name} [${f.id}] to parent ${a.new_parent_id}.`;
    },
  },

  delete: {
    description:
      'PERMANENTLY delete a file/folder (does NOT go to trash — cannot be undone). Deleting a folder removes its contents.',
    inputSchema: {
      type: 'object',
      properties: { file_id: { type: 'string' } },
      required: ['file_id'],
    },
    handler: async (a) => {
      await gapi(`${BASE}/files/${a.file_id}?${SHARED}`, { method: 'DELETE', raw: true });
      return `Permanently deleted ${a.file_id}.`;
    },
  },

  share: {
    description:
      'Share a file/folder. For a specific person pass email + role (reader|writer|commenter). For a public link pass type="anyone".',
    inputSchema: {
      type: 'object',
      properties: {
        file_id: { type: 'string' },
        email: { type: 'string', description: 'Grantee email (for type=user)' },
        role: { type: 'string', enum: ['reader', 'writer', 'commenter', 'owner'] },
        type: { type: 'string', enum: ['user', 'anyone'], description: 'Defaults to user' },
      },
      required: ['file_id'],
    },
    handler: async (a) => {
      const type = a.type || 'user';
      const role = a.role || 'reader';
      if (type === 'user' && !a.email) throw new Error('email is required for type=user.');
      const body = { type, role, ...(type === 'user' ? { emailAddress: a.email } : {}) };
      const perm = await gapi(
        `${BASE}/files/${a.file_id}/permissions?fields=id&sendNotificationEmail=false&${SHARED}`,
        { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) },
      );
      const meta = await gapi(`${BASE}/files/${a.file_id}?fields=webViewLink&${SHARED}`);
      return `Granted ${role} to ${type === 'user' ? a.email : 'anyone with the link'} (permission ${perm.id}).\nLink: ${meta.webViewLink || '(n/a)'}`;
    },
  },

  get_permissions: {
    description: 'List who has access to a file/folder.',
    inputSchema: {
      type: 'object',
      properties: { file_id: { type: 'string' } },
      required: ['file_id'],
    },
    handler: async (a) => {
      const data = await gapi(
        `${BASE}/files/${a.file_id}/permissions?fields=permissions(id,type,role,emailAddress,displayName)&${SHARED}`,
      );
      const perms = data.permissions || [];
      return perms.length
        ? perms.map((p) => `- [${p.id}] ${p.role} · ${p.type}${p.emailAddress ? ` · ${p.emailAddress}` : ''}`).join('\n')
        : 'No explicit permissions.';
    },
  },

  unshare: {
    description: 'Remove a permission (revoke access) by permission id. Use get_permissions to find ids.',
    inputSchema: {
      type: 'object',
      properties: {
        file_id: { type: 'string' },
        permission_id: { type: 'string' },
      },
      required: ['file_id', 'permission_id'],
    },
    handler: async (a) => {
      await gapi(`${BASE}/files/${a.file_id}/permissions/${a.permission_id}?${SHARED}`, {
        method: 'DELETE',
        raw: true,
      });
      return `Removed permission ${a.permission_id} from ${a.file_id}.`;
    },
  },
};

runServer({ serverInfo: { name: 'gdrive', version: '0.1.0' }, tools, multiAccount: true });
