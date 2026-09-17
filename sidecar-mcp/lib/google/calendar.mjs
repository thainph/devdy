// Google Calendar tool group for the built-in `google` MCP server.
//
// Exposed as `cal_*` (tools namespaced `mcp__google__cal_*`). Uses the granted
// `calendar.readonly` + `calendar.events` scopes, so it can read every calendar
// and create/update/delete EVENTS — but not create or delete calendars.
//
// Deliberate difference from the Calendar UI screen (`src-tauri/src/commands/
// gcalendar.rs`), which always sends `sendUpdates=all`: here the default is
// `none`. A human clicking Save in the UI knows invites go out; an agent
// reorganising a calendar should not silently email attendees. Callers opt in
// per call with `notify: true`.

import { accountCtx, gapi, getAccessToken } from '../google.mjs';

const BASE = 'https://www.googleapis.com/calendar/v3';
const enc = encodeURIComponent;

/** Google mails every attendee unless told otherwise — see the header note. */
const sendUpdates = (a) => (a.notify ? 'all' : 'none');

/** Calendar ids are email-like (`x@group.calendar.google.com`), so they must be
 *  percent-encoded when used as a path component. */
const calPath = (id) => enc(id || 'primary');

/** Accounts connected before Devdy asked for the Calendar scopes get a 401/403
 *  from every call; say so instead of leaking a raw Google error. */
function enrich(e) {
  const msg = String((e && e.message) || e);
  if (/→ 40[13]/.test(msg)) {
    return new Error(
      `${msg}\nHint: this Google account may predate Devdy's Calendar scopes. Reconnect it in Devdy Settings → Google Account.`,
    );
  }
  return e;
}

async function calApi(url, opts) {
  try {
    return await gapi(url, opts);
  } catch (e) {
    throw enrich(e);
  }
}

const jsonBody = (body) => ({
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

// Per-account cache of the user's own Calendar timezone. Without it every timed
// event would be created in UTC and land at the wrong hour.
const tzCache = new Map(); // account label ('' = default) → IANA zone

async function accountTimeZone() {
  const key = accountCtx.getStore() ?? '';
  if (tzCache.has(key)) return tzCache.get(key);
  let zone = 'UTC';
  try {
    const r = await gapi(`${BASE}/users/me/settings/timezone`);
    if (r && r.value) zone = r.value;
  } catch {
    /* fall back to UTC rather than failing the call */
  }
  tzCache.set(key, zone);
  return zone;
}

const isDateOnly = (s) => /^\d{4}-\d{2}-\d{2}$/.test(String(s || ''));

/** Build a Google `EventDateTime`. A bare `YYYY-MM-DD` means an all-day event;
 *  anything else is treated as RFC3339 and tagged with a timezone. */
async function eventDateTime(value, timezone) {
  if (isDateOnly(value)) return { date: value };
  return { dateTime: value, timeZone: timezone || (await accountTimeZone()) };
}

/** Resolve the [timeMin, timeMax) window shared by the read tools. */
function timeWindow(a) {
  const parsed = a.time_min ? Date.parse(a.time_min) : NaN;
  const timeMin = Number.isNaN(parsed) ? new Date().toISOString() : new Date(parsed).toISOString();
  if (a.time_max) return { timeMin, timeMax: a.time_max };
  const days = Math.min(Math.max(Number(a.days) || 7, 1), 365);
  return { timeMin, timeMax: new Date(Date.parse(timeMin) + days * 86_400_000).toISOString() };
}

const startOf = (ev) => (ev.start && (ev.start.dateTime || ev.start.date)) || '';

function fmtEvent(ev, calendarId) {
  const end = (ev.end && (ev.end.dateTime || ev.end.date)) || '?';
  const parts = [
    ev.location ? ` · ${ev.location}` : '',
    (ev.attendees || []).length ? ` · ${ev.attendees.length} attendee(s)` : '',
    ev.hangoutLink ? ` · ${ev.hangoutLink}` : '',
    ev.recurringEventId ? ' · recurring' : '',
  ].join('');
  return `- ${startOf(ev) || '?'} → ${end}  ${ev.summary || '(no title)'}  [${ev.id}]${
    calendarId ? ` · ${calendarId}` : ''
  }${parts}`;
}

/** Which calendars a read tool should hit: one id, or every calendar in the
 *  account when `calendar_id` is "all". */
async function targetCalendars(a) {
  const id = a.calendar_id || 'primary';
  if (id !== 'all') return [id];
  const data = await calApi(`${BASE}/users/me/calendarList?fields=items(id)&maxResults=250`);
  const ids = (data.items || []).map((c) => c.id);
  return ids.length ? ids : ['primary'];
}

/** Read events from one calendar. `singleEvents` expands recurring series into
 *  individual occurrences, which is what a human means by "my schedule". */
async function eventsFrom(calendarId, { timeMin, timeMax, q, limit }) {
  const params = [
    `timeMin=${enc(timeMin)}`,
    `timeMax=${enc(timeMax)}`,
    'singleEvents=true',
    'orderBy=startTime',
    `maxResults=${limit}`,
    ...(q ? [`q=${enc(q)}`] : []),
  ].join('&');
  const data = await calApi(`${BASE}/calendars/${calPath(calendarId)}/events?${params}`);
  return (data.items || [])
    .filter((ev) => ev.status !== 'cancelled' && startOf(ev))
    .map((ev) => ({ ev, calendarId }));
}

/** Shared read path for list_events / search_events.
 *
 *  Sweeping "all" calendars routinely hits one the account can't actually read
 *  (a stale subscription, a revoked share). One such calendar must not blank out
 *  the whole schedule, so failures are collected and reported beneath the events
 *  — the same trade-off the Calendar screen makes in gcalendar.rs. A single
 *  explicitly requested calendar still fails loudly. */
async function readEvents(a, q) {
  const { timeMin, timeMax } = timeWindow(a);
  const limit = Math.min(Math.max(Number(a.limit) || 50, 1), 250);
  const calendars = await targetCalendars(a);
  const settled = await Promise.allSettled(
    calendars.map((id) => eventsFrom(id, { timeMin, timeMax, q, limit })),
  );
  if (calendars.length === 1 && settled[0].status === 'rejected') throw settled[0].reason;

  const rows = [];
  const failures = [];
  settled.forEach((r, i) => {
    if (r.status === 'fulfilled') rows.push(...r.value);
    else failures.push(`  ! ${calendars[i]}: ${(r.reason && r.reason.message) || r.reason}`);
  });
  rows.sort((x, y) => startOf(x.ev).localeCompare(startOf(y.ev)));

  const multi = calendars.length > 1;
  const shown = rows.slice(0, limit);
  const lines = shown.length
    ? [
        `${shown.length} event(s) between ${timeMin} and ${timeMax}:`,
        ...shown.map((r) => fmtEvent(r.ev, multi ? r.calendarId : '')),
      ]
    : [`No events between ${timeMin} and ${timeMax}.`];
  if (failures.length) lines.push(`${failures.length} calendar(s) could not be read:`, ...failures);
  return lines.join('\n');
}

/** Map the caller's arguments onto a Google event resource. Only keys the caller
 *  actually passed are included, so `update_event` can PATCH partially. */
async function buildEvent(a) {
  const body = {};
  if (a.summary !== undefined) body.summary = a.summary;
  if (a.description !== undefined) body.description = a.description;
  if (a.location !== undefined) body.location = a.location;
  if (a.start !== undefined) body.start = await eventDateTime(a.start, a.timezone);
  if (a.end !== undefined) body.end = await eventDateTime(a.end, a.timezone);
  if (a.attendees !== undefined) {
    body.attendees = (a.attendees || []).map((email) => ({ email }));
  }
  if (a.recurrence !== undefined) body.recurrence = a.recurrence;
  if (a.reminders_minutes !== undefined) {
    body.reminders = {
      useDefault: false,
      overrides: (a.reminders_minutes || []).map((minutes) => ({ method: 'popup', minutes })),
    };
  }
  if (a.add_meet) {
    body.conferenceData = {
      createRequest: {
        requestId: `devdy-${process.pid}-${Date.now()}`,
        conferenceSolutionKey: { type: 'hangoutsMeet' },
      },
    };
  }
  return body;
}

const WRITE_QUERY = (a) => `conferenceDataVersion=1&sendUpdates=${sendUpdates(a)}`;

const NOTIFY_PROP = {
  type: 'boolean',
  description:
    'Email the attendees about this change. Defaults to false — Devdy stays silent unless you explicitly ask to notify.',
};
const CALENDAR_PROP = {
  type: 'string',
  description: 'Calendar id; defaults to "primary" (the account\'s own calendar).',
};
const TIMEZONE_PROP = {
  type: 'string',
  description: 'IANA zone for timed events, e.g. "Asia/Ho_Chi_Minh". Defaults to the account\'s Calendar timezone.',
};

export const tools = {
  list_calendars: {
    description:
      'List the calendars of the account: id, name, and accessRole (owner/writer = you can create events on it; reader/freeBusyReader = read-only).',
    inputSchema: { type: 'object', properties: {} },
    handler: async () => {
      const data = await calApi(
        `${BASE}/users/me/calendarList?fields=items(id,summary,primary,accessRole,timeZone)&maxResults=250`,
      );
      const items = data.items || [];
      if (!items.length) return 'No calendars.';
      return items
        .map(
          (c) =>
            `- ${c.summary || '(untitled)'}  [${c.id}]  ${c.accessRole || '?'}${c.primary ? ' · primary' : ''}${
              c.timeZone ? ` · ${c.timeZone}` : ''
            }`,
        )
        .join('\n');
    },
  },

  list_events: {
    description:
      'List events in a time window (defaults to the next 7 days). Recurring series are expanded into individual occurrences. Pass calendar_id="all" to sweep every calendar of the account.',
    inputSchema: {
      type: 'object',
      properties: {
        calendar_id: CALENDAR_PROP,
        time_min: { type: 'string', description: 'RFC3339 start of the window; defaults to now' },
        time_max: { type: 'string', description: 'RFC3339 end of the window; defaults to time_min + `days`' },
        days: { type: 'integer', minimum: 1, maximum: 365, description: 'Window length when time_max is omitted (default 7)' },
        limit: { type: 'integer', minimum: 1, maximum: 250 },
      },
    },
    handler: (a) => readEvents(a, undefined),
  },

  search_events: {
    description:
      'Free-text search over events (matches title, description, location, attendees). Same time window rules as cal_list_events.',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Free-text term' },
        calendar_id: CALENDAR_PROP,
        time_min: { type: 'string' },
        time_max: { type: 'string' },
        days: { type: 'integer', minimum: 1, maximum: 365, description: 'Default 7' },
        limit: { type: 'integer', minimum: 1, maximum: 250 },
      },
      required: ['query'],
    },
    handler: (a) => readEvents(a, a.query),
  },

  get_event: {
    description:
      'Full detail of one event as JSON: attendees with their RSVP status, recurrence rules, reminders, Meet link and attachments.',
    inputSchema: {
      type: 'object',
      properties: { calendar_id: CALENDAR_PROP, event_id: { type: 'string' } },
      required: ['event_id'],
    },
    handler: async (a) => {
      const ev = await calApi(`${BASE}/calendars/${calPath(a.calendar_id)}/events/${enc(a.event_id)}`);
      return JSON.stringify(ev, null, 2);
    },
  },

  freebusy: {
    description:
      'Check which time ranges are already busy, so you can propose a slot that actually works. Accepts several calendar ids (a colleague\'s email works if they share their calendar with you).',
    inputSchema: {
      type: 'object',
      properties: {
        time_min: { type: 'string', description: 'RFC3339 start; defaults to now' },
        time_max: { type: 'string', description: 'RFC3339 end; defaults to time_min + `days`' },
        days: { type: 'integer', minimum: 1, maximum: 60, description: 'Default 7' },
        calendar_ids: {
          type: 'array',
          items: { type: 'string' },
          description: 'Calendars/emails to check; defaults to ["primary"]',
        },
      },
    },
    handler: async (a) => {
      const { timeMin, timeMax } = timeWindow(a);
      const ids = (a.calendar_ids || []).length ? a.calendar_ids : ['primary'];
      const data = await calApi(`${BASE}/freeBusy`, {
        method: 'POST',
        ...jsonBody({ timeMin, timeMax, items: ids.map((id) => ({ id })) }),
      });
      const cals = data.calendars || {};
      const lines = [];
      for (const id of Object.keys(cals)) {
        const entry = cals[id] || {};
        if ((entry.errors || []).length) {
          lines.push(`- ${id}: unavailable (${entry.errors.map((e) => e.reason).join(', ')})`);
          continue;
        }
        const busy = entry.busy || [];
        lines.push(
          busy.length
            ? `- ${id}:\n${busy.map((b) => `    busy ${b.start} → ${b.end}`).join('\n')}`
            : `- ${id}: free for the whole window`,
        );
      }
      return [`Busy ranges between ${timeMin} and ${timeMax}:`, ...lines].join('\n');
    },
  },

  create_event: {
    description:
      'Create an event. For a timed event pass RFC3339 start/end; for an all-day event pass YYYY-MM-DD (end is EXCLUSIVE — a one-day event on the 5th ends on the 6th). Attendees are NOT emailed unless notify=true.',
    inputSchema: {
      type: 'object',
      properties: {
        calendar_id: CALENDAR_PROP,
        summary: { type: 'string', description: 'Event title' },
        start: { type: 'string', description: 'RFC3339 datetime, or YYYY-MM-DD for all-day' },
        end: { type: 'string', description: 'RFC3339 datetime, or YYYY-MM-DD (exclusive) for all-day' },
        timezone: TIMEZONE_PROP,
        description: { type: 'string' },
        location: { type: 'string' },
        attendees: { type: 'array', items: { type: 'string' }, description: 'Attendee email addresses' },
        recurrence: {
          type: 'array',
          items: { type: 'string' },
          description: 'RRULE lines, e.g. ["RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=10"]',
        },
        reminders_minutes: {
          type: 'array',
          items: { type: 'integer' },
          description: 'Popup reminders, in minutes before the start',
        },
        add_meet: { type: 'boolean', description: 'Attach a Google Meet link' },
        notify: NOTIFY_PROP,
      },
      required: ['summary', 'start', 'end'],
    },
    handler: async (a) => {
      const body = await buildEvent(a);
      const ev = await calApi(
        `${BASE}/calendars/${calPath(a.calendar_id)}/events?${WRITE_QUERY(a)}`,
        { method: 'POST', ...jsonBody(body) },
      );
      return [
        `Created "${ev.summary || '(no title)'}" [${ev.id}]`,
        `  ${startOf(ev)} → ${(ev.end && (ev.end.dateTime || ev.end.date)) || '?'}`,
        ev.hangoutLink ? `  Meet: ${ev.hangoutLink}` : '',
        ev.htmlLink ? `  Link: ${ev.htmlLink}` : '',
        a.notify ? '  Attendees were notified.' : '  Attendees were NOT notified (pass notify=true to send invites).',
      ]
        .filter(Boolean)
        .join('\n');
    },
  },

  update_event: {
    description:
      'Change an existing event. Only the fields you pass are touched — everything else keeps its current value. Attendees are NOT emailed unless notify=true.',
    inputSchema: {
      type: 'object',
      properties: {
        calendar_id: CALENDAR_PROP,
        event_id: { type: 'string' },
        summary: { type: 'string' },
        start: { type: 'string', description: 'RFC3339 datetime, or YYYY-MM-DD for all-day' },
        end: { type: 'string', description: 'RFC3339 datetime, or YYYY-MM-DD (exclusive) for all-day' },
        timezone: TIMEZONE_PROP,
        description: { type: 'string' },
        location: { type: 'string' },
        attendees: { type: 'array', items: { type: 'string' }, description: 'REPLACES the whole attendee list' },
        recurrence: { type: 'array', items: { type: 'string' } },
        reminders_minutes: { type: 'array', items: { type: 'integer' } },
        add_meet: { type: 'boolean' },
        notify: NOTIFY_PROP,
      },
      required: ['event_id'],
    },
    handler: async (a) => {
      const body = await buildEvent(a);
      if (!Object.keys(body).length) throw new Error('Nothing to update — pass at least one field to change.');
      const ev = await calApi(
        `${BASE}/calendars/${calPath(a.calendar_id)}/events/${enc(a.event_id)}?${WRITE_QUERY(a)}`,
        { method: 'PATCH', ...jsonBody(body) },
      );
      return `Updated "${ev.summary || '(no title)'}" [${ev.id}] — ${startOf(ev)} → ${
        (ev.end && (ev.end.dateTime || ev.end.date)) || '?'
      }${a.notify ? '\nAttendees were notified.' : ''}`;
    },
  },

  delete_event: {
    description:
      'Delete an event. Deleting an occurrence of a recurring series removes only that occurrence. Attendees are NOT emailed unless notify=true.',
    inputSchema: {
      type: 'object',
      properties: { calendar_id: CALENDAR_PROP, event_id: { type: 'string' }, notify: NOTIFY_PROP },
      required: ['event_id'],
    },
    handler: async (a) => {
      // Not gapi(): 410/404 mean "already gone", which is success here, and gapi
      // throws on every non-2xx.
      const token = await getAccessToken();
      const res = await fetch(
        `${BASE}/calendars/${calPath(a.calendar_id)}/events/${enc(a.event_id)}?sendUpdates=${sendUpdates(a)}`,
        { method: 'DELETE', headers: { Authorization: `Bearer ${token}` } },
      );
      if (res.status === 410 || res.status === 404) return `Event ${a.event_id} was already deleted.`;
      if (!res.ok) {
        throw enrich(
          new Error(`DELETE ${BASE}/calendars/.../events → ${res.status}: ${(await res.text()).slice(0, 300)}`),
        );
      }
      return `Deleted event ${a.event_id}.`;
    },
  },

  respond_event: {
    description:
      'RSVP to an invitation on behalf of the connected account (accepted / declined / tentative). Does not change anyone else\'s response.',
    inputSchema: {
      type: 'object',
      properties: {
        calendar_id: CALENDAR_PROP,
        event_id: { type: 'string' },
        status: { type: 'string', enum: ['accepted', 'declined', 'tentative'] },
        notify: NOTIFY_PROP,
      },
      required: ['event_id', 'status'],
    },
    handler: async (a) => {
      const path = `${BASE}/calendars/${calPath(a.calendar_id)}/events/${enc(a.event_id)}`;
      const ev = await calApi(`${path}?fields=summary,attendees`);
      const attendees = ev.attendees || [];
      // `self` is Google's own marker for the requesting account; fall back to an
      // email match against the primary calendar id (which IS the user's address).
      let mine = attendees.find((x) => x.self);
      if (!mine) {
        const me = (await calApi(`${BASE}/calendars/primary?fields=id`)).id || '';
        mine = attendees.find((x) => (x.email || '').toLowerCase() === me.toLowerCase());
      }
      if (!mine) throw new Error(`You are not an attendee of event ${a.event_id}, so there is nothing to RSVP to.`);
      mine.responseStatus = a.status;
      await calApi(`${path}?sendUpdates=${sendUpdates(a)}`, { method: 'PATCH', ...jsonBody({ attendees }) });
      return `RSVP'd ${a.status} to "${ev.summary || '(no title)'}" [${a.event_id}].`;
    },
  },
};
