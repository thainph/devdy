//! Read-only Google Calendar access for the unified Calendar screen.
//!
//! Unlike the built-in `gdrive`/`gmail` MCP servers (which run only during agent
//! runs), the Calendar view is a dedicated UI screen that needs synchronous data.
//! So we call the Google Calendar REST API directly from Rust: mint a short-lived
//! access token from each account's stored `refresh_token` (same pattern as the
//! token exchange in `google.rs`), list the account's calendars, then pull events
//! in the requested time window.
//!
//! Errors from one account never fail the whole screen — they are collected into
//! an `errors` array so the UI can flag accounts that need reconnecting (e.g. one
//! connected before the `calendar.readonly` scope was added).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::secrets;

/// Cap on simultaneous in-flight Google API requests (across all accounts and
/// calendars) so a user with many calendars doesn't trip Google's rate limits.
const MAX_CONCURRENCY: usize = 8;

const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const CALENDAR_LIST_ENDPOINT: &str =
    "https://www.googleapis.com/calendar/v3/users/me/calendarList";

/// One calendar belonging to a connected account.
#[derive(Serialize)]
pub struct CalendarMeta {
    pub account_id: String,
    pub account_label: String,
    pub calendar_id: String,
    pub summary: String,
    pub bg_color: Option<String>,
    pub primary: bool,
    /// Google `accessRole` for this calendar: "owner" | "writer" | "reader" |
    /// "freeBusyReader". FE uses it to filter writable calendars.
    pub access_role: Option<String>,
}

/// A single (flattened) calendar event across accounts/calendars.
#[derive(Serialize)]
pub struct CalEvent {
    pub id: String,
    pub account_id: String,
    pub account_label: String,
    pub calendar_id: String,
    pub calendar_summary: String,
    pub title: String,
    /// RFC3339 datetime for timed events, or `YYYY-MM-DD` for all-day events.
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub location: Option<String>,
    pub html_link: Option<String>,
    pub color: Option<String>,
    /// Raw description (may contain HTML) — sanitized on the frontend.
    pub description: Option<String>,
    /// Google Meet / video-conference join URL, if any.
    pub meet_link: Option<String>,
    pub attachments: Vec<CalAttachment>,
}

/// A Google Drive (or other) file attached to an event.
#[derive(Serialize)]
pub struct CalAttachment {
    pub file_url: Option<String>,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub icon_link: Option<String>,
}

/// One account that could not be read (missing scope, revoked token, network…).
#[derive(Serialize)]
pub struct AccountError {
    pub account_id: String,
    pub account_label: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct CalendarListResult {
    pub calendars: Vec<CalendarMeta>,
    pub errors: Vec<AccountError>,
}

#[derive(Serialize)]
pub struct EventsResult {
    pub events: Vec<CalEvent>,
    pub errors: Vec<AccountError>,
}

// ---- Google API response shapes -------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
}

#[derive(Deserialize)]
struct CalendarListEntry {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default, rename = "backgroundColor")]
    background_color: Option<String>,
    #[serde(default)]
    primary: bool,
    /// Whether the user shows this calendar in the Google Calendar UI. Used to
    /// skip subscribed/hidden calendars when fetching events (speed).
    #[serde(default)]
    selected: bool,
    #[serde(default, rename = "accessRole")]
    access_role: Option<String>,
}

#[derive(Deserialize)]
struct EventsResponse {
    #[serde(default)]
    items: Vec<EventEntry>,
}

#[derive(Deserialize)]
struct EventEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "htmlLink")]
    html_link: Option<String>,
    #[serde(default)]
    start: Option<EventDateTime>,
    #[serde(default)]
    end: Option<EventDateTime>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "hangoutLink")]
    hangout_link: Option<String>,
    #[serde(default, rename = "conferenceData")]
    conference_data: Option<ConferenceData>,
    #[serde(default)]
    attachments: Vec<AttachmentEntry>,
}

#[derive(Deserialize)]
struct EventDateTime {
    #[serde(default, rename = "dateTime")]
    date_time: Option<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
struct ConferenceData {
    #[serde(default, rename = "entryPoints")]
    entry_points: Vec<EntryPoint>,
}

#[derive(Deserialize)]
struct EntryPoint {
    #[serde(default, rename = "entryPointType")]
    entry_point_type: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Deserialize)]
struct AttachmentEntry {
    #[serde(default, rename = "fileUrl")]
    file_url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "mimeType")]
    mime_type: Option<String>,
    #[serde(default, rename = "iconLink")]
    icon_link: Option<String>,
}

// ---- Write payload DTOs (input from the frontend) --------------------------
//
// serde field names mirror the Google Calendar API v3 event resource so the
// payload can be forwarded straight into the request body. Every field is
// Option + `skip_serializing_if` so a PATCH (update) only sends the fields the
// FE actually changed (AC-05).

/// Create/update event payload.
#[derive(Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<EventDateTimePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<EventDateTimePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<Attendee>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<Reminders>,
    /// Each element is one RRULE/EXRULE/RDATE line
    /// (e.g. "RRULE:FREQ=WEEKLY;BYDAY=MO").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Vec<String>>,
    /// When the FE wants a Google Meet, it sends `conferenceData.createRequest`.
    /// Forwarded verbatim (backend stays decoupled from the Meet shape).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "conferenceData"
    )]
    pub conference_data: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventDateTimePayload {
    /// Timed event: RFC3339, e.g. "2026-08-11T09:00:00+07:00".
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "dateTime")]
    pub date_time: Option<String>,
    /// All-day: "YYYY-MM-DD" (end is exclusive per the Google API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// IANA tz, e.g. "Asia/Ho_Chi_Minh". Timed events only.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "timeZone")]
    pub time_zone: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Attendee {
    pub email: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminders {
    /// false when custom overrides are supplied; true to use calendar defaults.
    pub use_default: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ReminderOverride>,
}

#[derive(Deserialize, Serialize)]
pub struct ReminderOverride {
    /// "email" | "popup".
    pub method: String,
    /// Minutes before the event.
    pub minutes: i64,
}

// ---- Token minting (cached) ------------------------------------------------

/// Access-token cache keyed by account id → (token, expires_at). Google tokens
/// live ~1h; caching avoids a token POST on every navigation/refetch.
fn token_cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mint (or reuse a cached) short-lived access token for one account.
async fn access_token_for(client: &reqwest::Client, account_id: &str) -> Result<String, String> {
    // Fast path: a still-valid cached token.
    if let Ok(cache) = token_cache().lock() {
        if let Some((tok, exp)) = cache.get(account_id) {
            if *exp > Instant::now() {
                return Ok(tok.clone());
            }
        }
    }

    let creds = secrets::get_google_client()
        .ok_or_else(|| "No saved OAuth client credentials.".to_string())?;
    let refresh_token = secrets::get_google_account_token(account_id)
        .ok_or_else(|| "No stored token for this account.".to_string())?;

    let token: TokenResponse = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|e| format!("Token refresh request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Token refresh rejected (reconnect the account?): {e}"))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    // Cache for ~55 min (tokens are valid ~60).
    if let Ok(mut cache) = token_cache().lock() {
        cache.insert(
            account_id.to_string(),
            (token.access_token.clone(), Instant::now() + Duration::from_secs(55 * 60)),
        );
    }

    Ok(token.access_token)
}

async fn fetch_calendars(
    client: &reqwest::Client,
    token: &str,
) -> Result<Vec<CalendarListEntry>, String> {
    let resp: CalendarListResponse = client
        .get(CALENDAR_LIST_ENDPOINT)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("calendarList request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("calendarList rejected: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse calendarList: {e}"))?;
    Ok(resp.items)
}

/// (id, label) of the accounts to query — all, or the given subset.
async fn selected_accounts(
    db: &Db,
    account_ids: Option<Vec<String>>,
) -> Result<Vec<(String, String)>, String> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, label FROM google_accounts ORDER BY is_default DESC, label")
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;
    Ok(match account_ids {
        Some(ids) if !ids.is_empty() => {
            rows.into_iter().filter(|(id, _)| ids.contains(id)).collect()
        }
        _ => rows,
    })
}

// ---- Commands --------------------------------------------------------------

/// List every calendar of every (or the given) connected account.
#[tauri::command]
pub async fn list_google_calendars(
    db: State<'_, Db>,
    account_ids: Option<Vec<String>>,
) -> Result<CalendarListResult, String> {
    let accounts = selected_accounts(db.inner(), account_ids).await?;
    let client = reqwest::Client::new();
    let mut calendars = Vec::new();
    let mut errors = Vec::new();

    for (id, label) in accounts {
        match async {
            let token = access_token_for(&client, &id).await?;
            fetch_calendars(&client, &token).await
        }
        .await
        {
            Ok(items) => {
                for c in items {
                    calendars.push(CalendarMeta {
                        account_id: id.clone(),
                        account_label: label.clone(),
                        calendar_id: c.id,
                        summary: c.summary,
                        bg_color: c.background_color,
                        primary: c.primary,
                        access_role: c.access_role,
                    });
                }
            }
            Err(message) => errors.push(AccountError {
                account_id: id,
                account_label: label,
                message,
            }),
        }
    }

    Ok(CalendarListResult { calendars, errors })
}

/// Fetch and map one calendar's events in `[time_min, time_max)`.
async fn fetch_calendar_events(
    client: &reqwest::Client,
    token: &str,
    cal: &CalendarListEntry,
    account_id: &str,
    account_label: &str,
    time_min: &str,
    time_max: &str,
) -> Result<Vec<CalEvent>, String> {
    let url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/{}/events",
        urlencoding_component(&cal.id)
    );
    let resp: EventsResponse = client
        .get(&url)
        .bearer_auth(token)
        .query(&[
            ("timeMin", time_min),
            ("timeMax", time_max),
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
            ("maxResults", "2500"),
        ])
        .send()
        .await
        .map_err(|e| format!("events request failed for '{}': {e}", cal.summary))?
        .error_for_status()
        .map_err(|e| format!("events rejected for '{}': {e}", cal.summary))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse events for '{}': {e}", cal.summary))?;

    let mut out = Vec::new();
    for ev in resp.items {
        if let Some(mapped) = map_event_entry(
            ev,
            account_id,
            account_label,
            &cal.id,
            &cal.summary,
            cal.background_color.clone(),
        ) {
            out.push(mapped);
        }
    }
    Ok(out)
}

/// Map one Google event resource into a flattened `CalEvent`.
///
/// Shared by the read path (`fetch_calendar_events`) and the write path
/// (create/update). Returns `None` for cancelled events or events without a
/// usable start (unrenderable in a grid).
fn map_event_entry(
    ev: EventEntry,
    account_id: &str,
    account_label: &str,
    calendar_id: &str,
    calendar_summary: &str,
    color: Option<String>,
) -> Option<CalEvent> {
    if ev.status.as_deref() == Some("cancelled") {
        return None;
    }
    let (start, all_day_s) = normalize_dt(ev.start.as_ref());
    let (end, _) = normalize_dt(ev.end.as_ref());
    if start.is_empty() {
        return None; // events without a start are unusable in a grid
    }
    // Prefer the convenience hangoutLink; otherwise pull the first "video"
    // entry point from conferenceData.
    let meet_link = ev.hangout_link.clone().or_else(|| {
        ev.conference_data.as_ref().and_then(|c| {
            c.entry_points
                .iter()
                .find(|e| e.entry_point_type.as_deref() == Some("video"))
                .and_then(|e| e.uri.clone())
        })
    });
    let attachments = ev
        .attachments
        .into_iter()
        .map(|a| CalAttachment {
            file_url: a.file_url,
            title: a.title,
            mime_type: a.mime_type,
            icon_link: a.icon_link,
        })
        .collect();
    Some(CalEvent {
        id: ev.id,
        account_id: account_id.to_string(),
        account_label: account_label.to_string(),
        calendar_id: calendar_id.to_string(),
        calendar_summary: calendar_summary.to_string(),
        title: ev.summary.unwrap_or_else(|| "(no title)".to_string()),
        start,
        end,
        all_day: all_day_s,
        location: ev.location,
        html_link: ev.html_link,
        color,
        description: ev.description,
        meet_link,
        attachments,
    })
}

/// Look up an account's display label (empty string if not found). Create/update
/// need it to populate the returned `CalEvent`.
async fn account_label_for(db: &Db, account_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT label FROM google_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Fetch events in `[time_min, time_max)` (RFC3339) across the shown calendars of
/// the selected accounts. Accounts and calendars are fetched CONCURRENTLY (capped
/// at `MAX_CONCURRENCY`); per-account failures are collected in `errors`.
#[tauri::command]
pub async fn list_google_calendar_events(
    db: State<'_, Db>,
    account_ids: Option<Vec<String>>,
    time_min: String,
    time_max: String,
) -> Result<EventsResult, String> {
    let accounts = selected_accounts(db.inner(), account_ids).await?;
    let client = reqwest::Client::new();
    let sem = std::sync::Arc::new(Semaphore::new(MAX_CONCURRENCY));

    let account_futs = accounts.into_iter().map(|(id, label)| {
        let client = client.clone();
        let sem = sem.clone();
        let time_min = time_min.clone();
        let time_max = time_max.clone();
        async move {
            let res = async {
                let token = access_token_for(&client, &id).await?;
                let calendars = {
                    let _permit = sem.acquire().await.unwrap();
                    fetch_calendars(&client, &token).await?
                };
                // Only the calendars the user actually shows in Google (plus the
                // primary one) — skips holiday/birthday/subscribed calendars.
                let wanted: Vec<CalendarListEntry> = calendars
                    .into_iter()
                    .filter(|c| c.selected || c.primary)
                    .collect();

                let cal_futs = wanted.iter().map(|cal| {
                    let client = client.clone();
                    let sem = sem.clone();
                    let token = token.clone();
                    let id = id.clone();
                    let label = label.clone();
                    let time_min = time_min.clone();
                    let time_max = time_max.clone();
                    async move {
                        let _permit = sem.acquire().await.unwrap();
                        fetch_calendar_events(
                            &client, &token, cal, &id, &label, &time_min, &time_max,
                        )
                        .await
                    }
                });
                let per_cal = join_all(cal_futs).await;
                let mut out: Vec<CalEvent> = Vec::new();
                for r in per_cal {
                    out.extend(r?);
                }
                Ok::<Vec<CalEvent>, String>(out)
            }
            .await;
            (id, label, res)
        }
    });

    let results = join_all(account_futs).await;
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for (id, label, res) in results {
        match res {
            Ok(mut evs) => events.append(&mut evs),
            Err(message) => errors.push(AccountError {
                account_id: id,
                account_label: label,
                message,
            }),
        }
    }

    Ok(EventsResult { events, errors })
}

/// Normalize a Google event start/end into a string, returning `(value, all_day)`.
fn normalize_dt(dt: Option<&EventDateTime>) -> (String, bool) {
    match dt {
        Some(d) => {
            if let Some(dt) = &d.date_time {
                (dt.clone(), false)
            } else if let Some(date) = &d.date {
                (date.clone(), true)
            } else {
                (String::new(), false)
            }
        }
        None => (String::new(), false),
    }
}

/// Percent-encode a path component (calendar ids can contain `@`, `#`, etc.).
fn urlencoding_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
