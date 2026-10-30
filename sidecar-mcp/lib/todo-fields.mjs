// Todo field parsing / checks for the MCP todo tools (docs/focus-board-spec.md
// BR-01). Same rules as src-tauri/src/focus/validate.rs; the inputs are looser
// here (priority "P1", estimate "1h30") because an AI writes them.

export const STATUSES = ['todo', 'doing', 'waiting'];
export const ESTIMATE_MIN = 5;
export const ESTIMATE_MAX = 2400;

const blank = (v) => v == null || String(v).trim() === '';

/** A deadline as stored: `YYYY-MM-DD` (a real calendar date), or null to clear. */
export function normDue(due) {
  if (blank(due)) return null;
  const d = String(due).trim();
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(d);
  const date = m && new Date(Date.UTC(+m[1], +m[2] - 1, +m[3]));
  if (!date || date.toISOString().slice(0, 10) !== d) {
    throw new Error(`invalid due date (want YYYY-MM-DD): ${d}`);
  }
  return d;
}

/** 24 h `HH:MM`, or null to clear. */
export function normDueTime(t) {
  if (blank(t)) return null;
  const s = String(t).trim();
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(s)) throw new Error(`invalid due_time (want HH:MM, 24 h): ${s}`);
  return s;
}

/** 1–3, or "P1".."P3" (case-insensitive). */
export function parsePriority(p) {
  const m = /^p?([1-3])$/i.exec(String(p ?? '').trim());
  if (!m) throw new Error('priority must be 1, 2 or 3 (or "P1".."P3")');
  return Number(m[1]);
}

/**
 * Minutes from `90`, `"90"`, `"30m"`, `"2h"`, `"1.5h"`, `"1h30"`, `"1h 30m"`;
 * null to clear. Must land in [5, 2400].
 */
export function parseEstimate(e) {
  if (blank(e)) return null;
  const s = String(e).trim().toLowerCase();
  let min = null;
  let m;
  if (/^\d+$/.test(s)) min = Number(s);
  else if ((m = /^(\d+(?:\.\d+)?)\s*h(?:\s*(\d+)\s*m?)?$/.exec(s))) min = Math.round(Number(m[1]) * 60) + Number(m[2] ?? 0);
  else if ((m = /^(\d+)\s*m(?:in)?$/.exec(s))) min = Number(m[1]);
  if (min == null) throw new Error(`invalid estimate (want e.g. "30m", "2h", "1h30" or minutes): ${e}`);
  if (min < ESTIMATE_MIN || min > ESTIMATE_MAX) {
    throw new Error(`estimate must be between ${ESTIMATE_MIN} and ${ESTIMATE_MAX} minutes`);
  }
  return min;
}

export function normStatus(s) {
  const v = String(s ?? '').trim();
  if (!STATUSES.includes(v)) throw new Error('status must be todo, doing or waiting');
  return v;
}

/**
 * RFC3339 date-time (`2026-10-30T14:00:00+07:00`, `Z`; seconds optional and
 * added), or null to clear.
 */
export function normInstant(key, v) {
  if (blank(v)) return null;
  const s = String(v).trim();
  const m = /^(\d{4}-\d{2}-\d{2})[Tt](\d{2}:\d{2})(:\d{2}(?:\.\d+)?)?([Zz]|[+-]\d{2}:\d{2})$/.exec(s);
  if (!m || Number.isNaN(Date.parse(s))) {
    throw new Error(`${key} must be an RFC3339 date-time, e.g. 2026-10-30T14:00:00+07:00`);
  }
  return `${m[1]}T${m[2]}${m[3] ?? ':00'}${m[4].toUpperCase()}`;
}

/** `90` → `1h30`, `120` → `2h`, `45` → `45m` (list output). */
export function formatEstimate(min) {
  if (min == null) return null;
  const h = Math.floor(min / 60);
  const m = min % 60;
  if (!h) return `${m}m`;
  return m ? `${h}h${String(m).padStart(2, '0')}` : `${h}h`;
}
