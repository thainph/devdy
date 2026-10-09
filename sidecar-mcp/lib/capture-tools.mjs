// Tool definitions for captured content (Slack threads + web pages). Both kinds
// share one store (`captures`) and the same 10 tools; `captureTools(kind)` builds
// the `<prefix>_*` set for one kind so the surfaces can't drift apart.

import * as store from './store.mjs';
import { sliceMarkdown } from './markdown.mjs';

const READ_MAX_DEFAULT = 20000;

const KINDS = {
  slack: {
    prefix: 'slack_threads',
    noun: 'Slack thread',
    plural: 'Slack threads',
    what: 'Slack threads saved into Devdy by the Chrome extension (Slack thread inbox)',
    searchCols: 'title, body and channel',
    readMeta: 'metadata (workspace, channel, Slack URL…)',
  },
  web: {
    prefix: 'web_pages',
    noun: 'web page',
    plural: 'web pages',
    what: 'web pages captured into Devdy by the Chrome extension (page exported as markdown)',
    searchCols: 'title, body, site name and URL',
    readMeta: 'metadata (site, original URL, author, published/captured dates, whether it is only a selection)',
  },
};

const domainOf = (url) => {
  try {
    return new URL(url).hostname.replace(/^www\./, '');
  } catch {
    return null;
  }
};

/**
 * @param {'slack'|'web'} kind
 * @param {{ firstLine: Function, snippetAround: Function }} fmt shared formatters from index.mjs
 */
export function captureTools(kind, { firstLine, snippetAround }) {
  const k = KINDS[kind];
  const P = k.prefix;
  const titleOf = (c) => c.title || firstLine(c.content) || '(untitled)';
  // " · #channel · workspace · N attachment(s)" (slack) or " · site · excerpt · …" (web).
  const metaOf = (c) => {
    const parts =
      kind === 'slack'
        ? [c.channel, c.workspace]
        : [c.site_name || domainOf(c.source_url), c.selection ? 'excerpt' : null];
    const list = parts.filter(Boolean);
    if (c.attachment_count) list.push(`${c.attachment_count} attachment(s)`);
    return list.length ? ` · ${list.join(' · ')}` : '';
  };
  const detailMeta = (c) =>
    (kind === 'slack'
      ? [
          ['workspace', c.workspace],
          ['channel', c.channel],
          ['thread_url', c.thread_url],
          ['thread_ts', c.thread_ts],
          ['exported_at', c.exported_at],
        ]
      : [
          ['site', c.site_name || domainOf(c.source_url)],
          ['source_url', c.source_url],
          ['author', c.author],
          ['published_at', c.published_at],
          ['captured_at', c.exported_at],
          ['selection', c.selection ? 'yes — only the part the user selected, not the full page' : null],
          ['description', c.description],
        ]
    ).concat([
      ['title_custom', c.title_custom ? 'yes (kept on re-export)' : null],
      ['project_id', c.project_id],
      ['updated_at', c.updated_at],
    ]);

  return {
    [`${P}_list`]: {
      description: `List ${k.what}. Defaults to the current project; pass scope="all" for every project. Returns id, title, ${kind === 'slack' ? 'channel' : 'site'}, attachment count and a short preview.`,
      inputSchema: {
        type: 'object',
        properties: {
          scope: { type: 'string', enum: ['project', 'all'], description: 'project (default) or all' },
          limit: { type: 'integer', minimum: 1, maximum: 200 },
        },
      },
      handler: (a) => {
        const rows = store.listCaptures(kind, a);
        if (!rows.length) return `No ${k.plural} found.`;
        return rows
          .map((c) => {
            const title = titleOf(c);
            const preview = firstLine(c.content).slice(0, 120);
            return `- [${c.id}] ${title}${metaOf(c)}${preview && preview !== title ? ` — ${preview}` : ''} (updated ${c.updated_at})`;
          })
          .join('\n');
      },
    },

    [`${P}_read`]: {
      description: `Read one ${k.noun} by id: ${k.readMeta}, an outline of its markdown headings with char offsets, total_chars, its attachments with ABSOLUTE file paths, size and mime, then the markdown body. Long bodies are sliced: max_chars (default ${READ_MAX_DEFAULT}) + offset, or section="<heading text>" for just that section; a "continue with offset=N" hint is printed when truncated. Open attachments with ${P}_read_attachment or your own file reader.`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          max_chars: { type: 'integer', minimum: 1, description: `max body chars to return (default ${READ_MAX_DEFAULT})` },
          offset: { type: 'integer', minimum: 0, description: 'start char of the body slice (relative to the section when section is set)' },
          section: { type: 'string', description: 'heading text (case-insensitive) — return only that section' },
        },
        required: ['id'],
      },
      handler: (a) => {
        const c = store.readCapture(kind, a.id);
        if (!c) return `${k.noun[0].toUpperCase()}${k.noun.slice(1)} not found: ${a.id}`;
        const s = sliceMarkdown(c.content || '', {
          maxChars: a.max_chars ?? READ_MAX_DEFAULT,
          offset: a.offset ?? 0,
          section: a.section,
        });
        const meta = detailMeta(c)
          .filter(([, v]) => v)
          .map(([key, v]) => `- ${key}: ${v}`)
          .join('\n');
        const atts = c.attachments.length
          ? c.attachments
              .map((f) => `- [${f.id}] ${f.name} — ${f.path || `(invalid path: ${f.file_path})`} · ${f.size} bytes · ${f.mime || 'unknown mime'}`)
              .join('\n')
          : '(none)';
        const outlineText = s.headings.length
          ? s.headings.map((h) => `${'  '.repeat(h.level - 1)}- ${'#'.repeat(h.level)} ${h.text} (offset ${h.offset})`).join('\n')
          : '(no headings)';
        const scopeLabel = s.sectionHeading ? ` of section "${s.sectionHeading.text}"` : '';
        const range = `_chars ${s.start}–${s.end} of ${s.total}${scopeLabel}_`;
        let out = `# ${titleOf(c)}\n${meta}\n- total_chars: ${s.total}\n\n## Outline\n${outlineText}\n\n## Attachments (${c.attachments.length})\n${atts}\n\n---\n${range}\n\n${s.slice || '(empty)'}`;
        if (s.nextOffset != null) {
          const sec = s.sectionHeading ? ` section="${s.sectionHeading.text}"` : '';
          out += `\n\n---\n… truncated (${s.rangeEnd - s.end} chars left). Continue with offset=${s.nextOffset}${sec}.`;
        }
        return out;
      },
    },

    [`${P}_search`]: {
      description: `Search ${k.plural} by substring across ${k.searchCols}. Defaults to the current project; pass scope="all" for every project.`,
      inputSchema: {
        type: 'object',
        properties: {
          query: { type: 'string' },
          scope: { type: 'string', enum: ['project', 'all'] },
          limit: { type: 'integer', minimum: 1, maximum: 200 },
        },
        required: ['query'],
      },
      handler: (a) => {
        const rows = store.searchCaptures(kind, a);
        if (!rows.length) return `No ${k.plural} match "${String(a.query).trim()}".`;
        return rows
          .map((c) => `- [${c.id}] ${titleOf(c)}${metaOf(c)} — ${snippetAround(c.content, a.query)} (updated ${c.updated_at})`)
          .join('\n');
      },
    },

    [`${P}_rename`]: {
      description: `Rename a ${k.noun} (1–200 chars, trimmed). The name is marked custom, so it survives later re-exports of the same ${k.noun} from the Chrome extension.`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          title: { type: 'string', description: 'New title (max 200 characters)' },
        },
        required: ['id', 'title'],
      },
      handler: (a) => {
        const r = store.renameCapture(kind, a);
        return `Renamed ${k.noun} ${r.id} to "${r.title}".`;
      },
    },

    [`${P}_update`]: {
      description: `Replace a ${k.noun}'s title and/or markdown content. Omitted fields are left unchanged. A changed title is marked custom and survives re-exports (prefer ${P}_rename for title-only changes).`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          title: { type: 'string' },
          content: { type: 'string' },
        },
        required: ['id'],
      },
      handler: (a) => {
        store.updateCapture(kind, a);
        return `Updated ${k.noun} ${a.id}.`;
      },
    },

    [`${P}_append`]: {
      description: `Append a markdown block to a ${k.noun}'s content (adds a blank line separator), e.g. a summary or follow-up notes.`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          text: { type: 'string' },
        },
        required: ['id', 'text'],
      },
      handler: (a) => {
        store.appendCapture(kind, a);
        return `Appended to ${k.noun} ${a.id}.`;
      },
    },

    [`${P}_delete`]: {
      description: `Delete one or more ${k.plural} by id (permanent) — also removes their attachment files from disk. Pass a single id or a list; the DB batch is applied in one transaction.`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          ids: { type: 'array', items: { type: 'string' }, description: 'Bulk delete' },
        },
      },
      handler: (a) => {
        const r = store.deleteCaptures(kind, { ids: a.ids ?? a.id });
        return `Deleted ${r.deleted}/${r.requested} ${k.noun}(s) (${r.dirs_removed} attachment folder(s) removed).`;
      },
    },

    [`${P}_set_project`]: {
      description: `Link a ${k.noun} to the current project, or detach it with scope="global".`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          scope: { type: 'string', enum: ['project', 'global'] },
        },
        required: ['id'],
      },
      handler: (a) => {
        const r = store.setCaptureProject(kind, a);
        return `${k.noun[0].toUpperCase()}${k.noun.slice(1)} ${r.id} is now ${r.project_id ? `linked to project ${r.project_id}` : 'global (no project)'}.`;
      },
    },

    [`${P}_reorder`]: {
      description: `Reorder ${k.plural}. The given ids move to the top in the given order; every other ${k.noun} keeps its relative order below. Pass the full list for an exact ordering.`,
      inputSchema: {
        type: 'object',
        properties: { ids: { type: 'array', items: { type: 'string' } } },
        required: ['ids'],
      },
      handler: (a) => {
        const r = store.reorderCaptures(kind, a);
        return `Reordered ${r.ordered} ${k.noun}(s).`;
      },
    },

    [`${P}_read_attachment`]: {
      description: `Read one ${k.noun} attachment by attachment id (from ${P}_read). UTF-8 text files up to 1 MB are returned inline; otherwise (images, PDFs, large/binary files) returns the absolute path and mime so you can open it with your own file reader.`,
      inputSchema: {
        type: 'object',
        properties: { id: { type: 'string', description: 'attachment id' } },
        required: ['id'],
      },
      handler: (a) => {
        const f = store.readCaptureAttachment(kind, a.id);
        const head = `# ${f.name}\n- path: ${f.path}\n- size: ${f.size} bytes\n- mime: ${f.mime || 'unknown'}\n- ${k.noun}: ${f.capture_id}${f.capture_title ? ` (${f.capture_title})` : ''}`;
        if (f.text != null) return `${head}\n\n---\n\n${f.text || '(empty file)'}`;
        return `${head}\n\nNot returned inline: ${f.reason}. Use your file reader (e.g. the Read tool) on the absolute path above.`;
      },
    },
  };
}
