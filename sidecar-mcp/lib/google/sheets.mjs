// Google Sheets tool group for the built-in `google` MCP server.
//
// Exposed as `sheets_*` (tools namespaced `mcp__google__sheets_*`). Exists
// because Drive alone can't work with a spreadsheet: export only yields the
// first tab as CSV, and a media upload replaces the whole file (every other tab,
// formatting, validation and formulas are lost). These go through the Sheets
// API instead, so reads cover any tab and writes touch only the given cells —
// formatting, dropdowns and other tabs stay intact.
//
// The `drive` OAuth scope already covers the Sheets API; the Google Cloud
// project behind the OAuth client must have the Sheets API enabled.

import { gapi } from '../google.mjs';

const BASE = 'https://sheets.googleapis.com/v4/spreadsheets';
const enc = encodeURIComponent;
const DEFAULT_MAX_ROWS = 500;

/** Accept a bare id or a docs.google.com URL; a URL may also carry `gid`. */
export function parseSpreadsheet(input) {
  const s = String(input || '').trim();
  if (!s) throw new Error('spreadsheet_id is required.');
  const m = s.match(/\/spreadsheets\/d\/([a-zA-Z0-9_-]+)/);
  if (!m) return { id: encodeURIComponent(s), gid: null };
  const g = s.match(/[#&?]gid=(\d+)/);
  return { id: m[1], gid: g ? Number(g[1]) : null };
}

/** `'tab name'!A1:B2` — sheet titles are always quoted, `'` doubled. */
export function qualify(sheet, range) {
  const q = `'${String(sheet).replace(/'/g, "''")}'`;
  return range ? `${q}!${range}` : q;
}

/** 0 → A, 25 → Z, 26 → AA. */
export function colLetter(n) {
  let s = '';
  for (let i = n + 1; i > 0; i = Math.floor((i - 1) / 26)) s = String.fromCharCode(65 + ((i - 1) % 26)) + s;
  return s;
}

/** A → 0, AA → 26. */
function colIndex(letters) {
  let n = 0;
  for (const ch of letters.toUpperCase()) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n - 1;
}

/** Top-left cell of an API-returned range (`'tab'!C5:F9`) as 0-based indices. */
export function rangeOrigin(range) {
  const cell = String(range || '').split('!').pop().split(':')[0];
  const m = cell.match(/^([A-Za-z]*)(\d*)$/);
  return {
    col: m && m[1] ? colIndex(m[1]) : 0,
    row: m && m[2] ? Number(m[2]) - 1 : 0,
  };
}

const cellText = (v) => String(v ?? '').replace(/\t/g, ' ').replace(/\r?\n/g, '\\n');

/** One value range as a TSV grid labelled with real column letters and row
 *  numbers, so a cell can be addressed directly for sheets_update. */
export function formatGrid(vr, maxRows) {
  const rows = vr.values || [];
  const head = `## ${vr.range}`;
  if (!rows.length) return `${head}\n(empty)`;
  const { col, row } = rangeOrigin(vr.range);
  const width = Math.max(...rows.map((r) => r.length));
  const shown = rows.slice(0, maxRows);
  const lines = [
    ['row', ...Array.from({ length: width }, (_, i) => colLetter(col + i))].join('\t'),
    ...shown.map((r, i) => [row + i + 1, ...r.map(cellText)].join('\t')),
  ];
  if (rows.length > shown.length) {
    lines.push(`… ${rows.length - shown.length} more row(s) — read a narrower range or raise max_rows.`);
  }
  return `${head}\n${lines.join('\n')}`;
}

/** Sheets-specific hint for the most common setup failure. */
async function sheetsApi(url, opts) {
  try {
    return await gapi(url, opts);
  } catch (e) {
    const msg = e && e.message ? e.message : String(e);
    if (/SERVICE_DISABLED|has not been used|is disabled/i.test(msg)) {
      throw new Error(`${msg}\nEnable the Google Sheets API in the Google Cloud project that owns Devdy's OAuth client, then retry.`);
    }
    throw e;
  }
}

async function getTabs(id) {
  const data = await sheetsApi(
    `${BASE}/${id}?fields=properties.title,sheets.properties(sheetId,title,index,hidden,sheetType,gridProperties(rowCount,columnCount))`,
  );
  return { title: data.properties && data.properties.title, tabs: (data.sheets || []).map((s) => s.properties) };
}

const RENDER = {
  formatted: 'FORMATTED_VALUE',
  unformatted: 'UNFORMATTED_VALUE',
  formula: 'FORMULA',
};

export const tools = {
  info: {
    description:
      'List the tabs of a Google Sheet (title, sheetId/gid, size, hidden). Accepts a spreadsheet id or its docs.google.com URL. Use before sheets_read / sheets_update to get exact tab names.',
    inputSchema: {
      type: 'object',
      properties: { spreadsheet_id: { type: 'string', description: 'Spreadsheet id or URL' } },
      required: ['spreadsheet_id'],
    },
    handler: async (a) => {
      const { id } = parseSpreadsheet(a.spreadsheet_id);
      const { title, tabs } = await getTabs(id);
      const rows = tabs.map((t) => {
        const g = t.gridProperties || {};
        const kind = t.sheetType && t.sheetType !== 'GRID' ? ` · ${t.sheetType}` : '';
        return `- [${t.index}] ${t.title}  (gid ${t.sheetId}) · ${g.rowCount ?? '?'}×${g.columnCount ?? '?'}${t.hidden ? ' · hidden' : ''}${kind}`;
      });
      return `${title || id} — ${tabs.length} tab(s)\n${rows.join('\n')}`;
    },
  },

  read: {
    description:
      'Read cell values from any tab of a Google Sheet, as a grid labelled with column letters and row numbers. Give `sheet` (tab name) and/or an A1 `range`; with neither, a URL\'s gid picks the tab, otherwise every tab is read. Unlike drive_read (first tab only, CSV), this reaches all tabs.',
    inputSchema: {
      type: 'object',
      properties: {
        spreadsheet_id: { type: 'string', description: 'Spreadsheet id or URL' },
        sheet: { type: 'string', description: 'Tab name, e.g. "脆弱性の洗い出し"' },
        range: {
          type: 'string',
          description: 'A1 range within `sheet` (e.g. "A1:H200"), or a fully-qualified one ("\'Tab\'!A1:H200") when `sheet` is omitted',
        },
        render: {
          type: 'string',
          enum: ['formatted', 'unformatted', 'formula'],
          description: 'formatted (as displayed, default), unformatted (raw values), formula (show formulas)',
        },
        max_rows: { type: 'integer', minimum: 1, maximum: 5000, description: `Rows shown per tab (default ${DEFAULT_MAX_ROWS})` },
      },
      required: ['spreadsheet_id'],
    },
    handler: async (a) => {
      const { id, gid } = parseSpreadsheet(a.spreadsheet_id);
      const maxRows = Math.min(Math.max(Number(a.max_rows) || DEFAULT_MAX_ROWS, 1), 5000);
      let ranges;
      if (a.sheet) {
        ranges = [qualify(a.sheet, a.range)];
      } else if (a.range) {
        ranges = [a.range];
      } else {
        const { tabs } = await getTabs(id);
        const grids = tabs.filter((t) => !t.sheetType || t.sheetType === 'GRID');
        const picked = gid != null ? grids.filter((t) => t.sheetId === gid) : grids;
        if (!picked.length) throw new Error(gid != null ? `No tab with gid ${gid}.` : 'Spreadsheet has no grid tabs.');
        ranges = picked.map((t) => qualify(t.title));
      }
      const render = RENDER[a.render] || RENDER.formatted;
      const qs = ranges.map((r) => `ranges=${enc(r)}`).join('&');
      const data = await sheetsApi(`${BASE}/${id}/values:batchGet?${qs}&valueRenderOption=${render}&majorDimension=ROWS`);
      return (data.valueRanges || []).map((vr) => formatGrid(vr, maxRows)).join('\n\n');
    },
  },

  update: {
    description:
      'Write values into specific cells of a Google Sheet. Only the listed cells change — formatting, dropdowns/validation, other cells and other tabs are left as they are. Each entry is an A1 range (qualify with the tab: "\'Tab\'!G5" or "\'Tab\'!G5:H6") plus a 2-D array of values (rows of cells). Values are parsed as if typed (formulas, numbers, dates) unless raw=true.',
    inputSchema: {
      type: 'object',
      properties: {
        spreadsheet_id: { type: 'string', description: 'Spreadsheet id or URL' },
        data: {
          type: 'array',
          minItems: 1,
          items: {
            type: 'object',
            properties: {
              range: { type: 'string', description: 'A1 range, e.g. "\'脆弱性の洗い出し\'!G5"' },
              values: {
                type: 'array',
                items: { type: 'array', items: {} },
                description: 'Rows of cell values, e.g. [["Go"]] for one cell',
              },
            },
            required: ['range', 'values'],
          },
        },
        raw: { type: 'boolean', description: 'Store values literally (no formula/number parsing). Defaults to false.' },
      },
      required: ['spreadsheet_id', 'data'],
    },
    handler: async (a) => {
      const { id } = parseSpreadsheet(a.spreadsheet_id);
      if (!Array.isArray(a.data) || !a.data.length) throw new Error('data must be a non-empty array.');
      const res = await sheetsApi(`${BASE}/${id}/values:batchUpdate`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          valueInputOption: a.raw ? 'RAW' : 'USER_ENTERED',
          data: a.data.map((d) => ({ range: d.range, majorDimension: 'ROWS', values: d.values })),
        }),
      });
      const parts = (res.responses || []).map((r) => `- ${r.updatedRange} (${r.updatedCells || 0} cell(s))`);
      return `Updated ${res.totalUpdatedCells || 0} cell(s).\n${parts.join('\n')}`;
    },
  },

  append: {
    description:
      'Append rows after the last row of data in a tab (new rows are inserted, existing cells are not overwritten). `range` is the tab or table to append to, e.g. "\'Tab\'!A:H".',
    inputSchema: {
      type: 'object',
      properties: {
        spreadsheet_id: { type: 'string', description: 'Spreadsheet id or URL' },
        range: { type: 'string', description: 'Tab or table range, e.g. "\'Tab\'!A:H"' },
        values: { type: 'array', items: { type: 'array', items: {} }, description: 'Rows to append' },
        raw: { type: 'boolean', description: 'Store values literally (no formula/number parsing). Defaults to false.' },
      },
      required: ['spreadsheet_id', 'range', 'values'],
    },
    handler: async (a) => {
      const { id } = parseSpreadsheet(a.spreadsheet_id);
      const input = a.raw ? 'RAW' : 'USER_ENTERED';
      const res = await sheetsApi(
        `${BASE}/${id}/values/${enc(a.range)}:append?valueInputOption=${input}&insertDataOption=INSERT_ROWS`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ majorDimension: 'ROWS', values: a.values }),
        },
      );
      const u = res.updates || {};
      return `Appended ${u.updatedRows || 0} row(s) → ${u.updatedRange || a.range}.`;
    },
  },
};
