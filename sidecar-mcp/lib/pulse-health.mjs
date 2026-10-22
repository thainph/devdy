// Health of watches and projects for Project Pulse (BR-08/09/10/12).
//
// Mirror of src-tauri/src/pulse/health.rs: both sides run the shared vectors in
// src-tauri/src/pulse/health_vectors.json, so keep them in lockstep. An all-day
// deadline is stored as 23:59:59 local of its day, so every deadline compares by
// `due_at` directly: overdue = due_at < now, due soon = within the next 72 h.

const HOUR = 3600_000;
const DAY = 24 * HOUR;
const DUE_SOON = 72 * HOUR;
const MY_BALL_LATE = 24 * HOUR;
const CUSTOMER_SLOW = 5 * DAY;
const DONE_SETTLED = 7 * DAY;

/** RFC3339 → epoch ms, or null. */
const parse = (s) => {
  if (typeof s !== 'string' || !/^\d{4}-\d{2}-\d{2}T/.test(s.trim())) return null;
  const t = Date.parse(s.trim());
  return Number.isFinite(t) ? t : null;
};

function doneSettled(history, now) {
  let oldest = null;
  for (const d of history) {
    if (d.status !== 'done') break;
    oldest = parse(d.created_at) ?? oldest;
  }
  return oldest != null && now - oldest >= DONE_SETTLED;
}

/**
 * BR-08 for one watch: `{ health, finished, overdue, needs_me, waiting_customer, next_due }`.
 * `w = { last_error, history }`, history = digests newest first.
 */
export function watchHealth(w, now) {
  const d = w.history?.[0];
  if (!d) {
    return {
      health: w.last_error ? 'yellow' : 'pending',
      finished: false,
      overdue: false,
      needs_me: false,
      waiting_customer: false,
      next_due: null,
    };
  }
  const done = d.status === 'done';
  const ends = (d.deadlines ?? []).map((x) => ({ t: parse(x?.due_at), x })).filter((e) => e.t != null);
  const overdue = !done && ends.some((e) => e.t < now);
  const upcoming = ends.filter((e) => e.t >= now).sort((a, b) => a.t - b.t)[0];
  const dueSoon = !done && ends.some((e) => e.t >= now && e.t - now <= DUE_SOON);
  const since = parse(d.ball_since);
  const myLate = d.ball === 'me' && since != null && now - since > MY_BALL_LATE;
  const customerSlow = d.ball === 'customer' && since != null && now - since >= CUSTOMER_SLOW;
  let health;
  if (done) health = 'done';
  else if (d.status === 'blocked' || overdue || myLate) health = 'red';
  else if (dueSoon || d.status === 'at_risk' || customerSlow || w.last_error) health = 'yellow';
  else health = 'green';
  return {
    health,
    finished: done && (!!d.source_closed || doneSettled(w.history, now)),
    overdue,
    needs_me: !done && d.ball === 'me',
    waiting_customer: !done && d.ball === 'customer',
    next_due:
      !done && upcoming ? { title: upcoming.x.title ?? '', due_at: upcoming.x.due_at, all_day: !!upcoming.x.all_day } : null,
  };
}

/** BR-09 / BR-10: `{ health, next_milestone, counts }` from watch healths. */
export function projectHealth(watches) {
  const has = (h) => watches.some((w) => w.health === h);
  let health = 'none';
  if (has('red')) health = 'red';
  else if (has('yellow')) health = 'yellow';
  else if (watches.some((w) => w.health !== 'done')) health = 'green';
  const next = watches
    .map((w) => w.next_due)
    .filter((n) => n && parse(n.due_at) != null)
    .sort((a, b) => parse(a.due_at) - parse(b.due_at))[0];
  return {
    health,
    next_milestone: next ?? null,
    counts: {
      needs_me: watches.filter((w) => w.needs_me).length,
      waiting_customer: watches.filter((w) => w.waiting_customer).length,
      overdue: watches.filter((w) => w.overdue).length,
      watching: watches.length,
    },
  };
}

const RANK = { red: 0, yellow: 1, green: 2, pending: 2, done: 3, none: 3 };

/** Card order: health, nearest milestone (none last), name A→Z. */
export function compareProjects(a, b) {
  const r = RANK[a.health] - RANK[b.health];
  if (r) return r;
  const ta = parse(a.next_milestone?.due_at);
  const tb = parse(b.next_milestone?.due_at);
  if (ta != null && tb != null && ta !== tb) return ta - tb;
  if ((ta == null) !== (tb == null)) return ta == null ? 1 : -1;
  return a.name.toLowerCase().localeCompare(b.name.toLowerCase());
}

/** Row order inside a card: red, yellow, green / pending, done; then nearest deadline. */
export function compareWatches(a, b) {
  const r = RANK[a.health] - RANK[b.health];
  if (r) return r;
  const ta = parse(a.next_due?.due_at);
  const tb = parse(b.next_due?.due_at);
  if (ta != null && tb != null) return ta - tb;
  if ((ta == null) !== (tb == null)) return ta == null ? 1 : -1;
  return 0;
}

export const HEALTH_ICON = { red: '🔴', yellow: '🟡', green: '🟢', done: '✅', pending: '⏳', none: '⚪' };
