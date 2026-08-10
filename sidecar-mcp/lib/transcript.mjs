// Tolerant readers for Devdy run transcripts.
//
// Devdy persists each turn to two possible places:
//   1. `<project>/.devdy/runs/<run_id>.log` — NDJSON stream-json written by the
//      start/resume drain task (both engines).
//   2. the shared transcript store the CLI / IDE also write (Claude JSONL under
//      ~/.claude/projects, Codex rollout JSONL) — cached per run in
//      `runs.transcript_path`.
// The shapes differ slightly, so `extractMessage` is deliberately permissive:
// it digs out {role, text} from whatever nesting a line uses and skips the rest.

import fs from 'node:fs';
import path from 'node:path';

/** Flatten a message `content` (string | block[]) into plain text. */
function blockText(content) {
  if (content == null) return '';
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .map((b) => {
      if (typeof b === 'string') return b;
      if (!b || typeof b !== 'object') return '';
      if (b.type === 'text') return b.text || '';
      if (b.type === 'tool_use') {
        const input = b.input ? ` ${JSON.stringify(b.input).slice(0, 400)}` : '';
        return `[tool_use: ${b.name || '?'}${input}]`;
      }
      if (b.type === 'tool_result') {
        return `[tool_result] ${blockText(b.content)}`.trim();
      }
      if (typeof b.text === 'string') return b.text;
      return '';
    })
    .filter(Boolean)
    .join('\n');
}

/** Pull a {role, text} pair out of one parsed NDJSON line, or null. */
function extractMessage(obj) {
  if (!obj || typeof obj !== 'object') return null;
  const m = obj.message && typeof obj.message === 'object' ? obj.message : obj;
  const role = m.role || obj.role || obj.type || '';
  const content = m.content ?? obj.content ?? obj.text;
  const text = blockText(content).trim();
  if (!text) return null;
  // Skip bookkeeping-only entries with no conversational content.
  if (role === 'system' && text.length < 2) return null;
  return { role: String(role || 'unknown'), text };
}

/** Parse an NDJSON transcript file into an ordered list of {role, text}. */
export function parseTranscript(raw) {
  const out = [];
  for (const line of raw.split('\n')) {
    const s = line.trim();
    if (!s) continue;
    let obj;
    try {
      obj = JSON.parse(s);
    } catch {
      continue;
    }
    const msg = extractMessage(obj);
    if (msg) out.push(msg);
  }
  return out;
}

/**
 * Resolve and read a run's transcript. `projectPath` is the run's project dir
 * (from the joined `projects.path`). Returns {messages, source} or null.
 */
export function readRunTranscript(run, projectPath) {
  const candidates = [];
  if (projectPath) {
    candidates.push(path.join(projectPath, '.devdy', 'runs', `${run.id}.log`));
  }
  if (run.transcript_path) candidates.push(run.transcript_path);
  for (const file of candidates) {
    try {
      if (file && fs.existsSync(file)) {
        const raw = fs.readFileSync(file, 'utf8');
        const messages = parseTranscript(raw);
        if (messages.length) return { messages, source: file };
      }
    } catch {
      // Unreadable candidate — try the next one.
    }
  }
  return null;
}

/** Render messages as a readable markdown transcript, capped at `maxChars`. */
export function renderTranscript(messages, maxChars = 8000) {
  const parts = messages.map((m) => `### ${m.role}\n${m.text}`);
  let text = parts.join('\n\n');
  if (text.length > maxChars) {
    text = `${text.slice(0, maxChars)}\n\n… [truncated, ${text.length - maxChars} more chars]`;
  }
  return text;
}

/** Find the first match of `query` in the transcript and return a snippet. */
export function findSnippet(messages, query, window = 220) {
  const q = query.toLowerCase();
  for (const m of messages) {
    const idx = m.text.toLowerCase().indexOf(q);
    if (idx >= 0) {
      const start = Math.max(0, idx - window / 2);
      const end = Math.min(m.text.length, idx + query.length + window / 2);
      const snippet = m.text.slice(start, end).replace(/\s+/g, ' ').trim();
      return { role: m.role, snippet: `${start > 0 ? '…' : ''}${snippet}${end < m.text.length ? '…' : ''}` };
    }
  }
  return null;
}
