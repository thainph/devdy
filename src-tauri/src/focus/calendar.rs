//! Today's Google Calendar events for the Focus capacity bar (spec §3.5).
//!
//! The account is `focus.calendar_account_id`, else the default Google
//! account; the calendars are `focus.calendar_ids`, else `primary`. Results
//! (or the error) are cached 10 minutes per account + calendars + date;
//! `invalidate` busts the cache (the refresh button). `focus_board` never
//! waits on Google for more than 3 s: a slower fetch keeps running, fills the
//! cache and emits `focus:changed` so the board reloads; meanwhile the last
//! result for the same day (stale) is used.

use super::capacity::CalEvent;
use super::settings::FocusSettings;
use crate::commands::gcalendar::{default_google_account, fetch_busy_events, BusyEvent};
use crate::db::Db;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

const TTL: Duration = Duration::from_secs(10 * 60);
const WAIT: Duration = Duration::from_secs(3);

/// No Google account linked: the UI shows "connect Google Calendar".
pub const ERR_NO_ACCOUNT: &str = "no_account";
/// First fetch of the day still running after the 3 s budget.
pub const ERR_LOADING: &str = "loading";

#[derive(Clone)]
struct Entry {
    key: String,
    date: NaiveDate,
    at: Instant,
    result: Result<Vec<BusyEvent>, String>,
}

struct Inflight {
    key: String,
    done: watch::Receiver<bool>,
    /// A caller stopped waiting: announce the result when it lands.
    late: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    entry: Option<Entry>,
    inflight: Option<Inflight>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// Forget the cached events (next board fetches again).
pub fn invalidate() {
    if let Ok(mut s) = state().lock() {
        s.entry = None;
    }
}

fn calendars(settings: &FocusSettings) -> Vec<String> {
    if settings.calendar_ids.is_empty() {
        vec!["primary".to_string()]
    } else {
        settings.calendar_ids.clone()
    }
}

async fn fetch_all(account: &str, cals: &[String], day: NaiveDate) -> Result<Vec<BusyEvent>, String> {
    let bound = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|t| t.to_rfc3339())
            .unwrap_or_default()
    };
    let (min, max) = (bound(day), bound(day.succ_opt().unwrap_or(day)));
    let mut out = Vec::new();
    for cal in cals {
        out.extend(fetch_busy_events(account, cal, &min, &max).await?);
    }
    Ok(out)
}

/// Today's events, or an error code / message (`ERR_NO_ACCOUNT`,
/// `ERR_LOADING`, or the Google error). `app` gets `focus:changed` when a
/// fetch outlives the 3 s budget.
pub async fn today_events(
    db: &Db,
    settings: &FocusSettings,
    app: Option<AppHandle>,
) -> Result<Vec<BusyEvent>, String> {
    let account = match settings.calendar_account_id.clone() {
        Some(a) => a,
        None => default_google_account(db).await.ok_or_else(|| ERR_NO_ACCOUNT.to_string())?,
    };
    let cals = calendars(settings);
    let today = Local::now().date_naive();
    let key = format!("{account}|{}|{today}", cals.join(","));

    let (mut done, late, stale) = {
        let mut s = state().lock().map_err(|e| e.to_string())?;
        if let Some(e) = s.entry.as_ref().filter(|e| e.key == key && e.at.elapsed() < TTL) {
            return e.result.clone();
        }
        let stale = s
            .entry
            .as_ref()
            .filter(|e| e.date == today && e.key == key)
            .and_then(|e| e.result.clone().ok());
        let running = s.inflight.as_ref().filter(|i| i.key == key);
        match running {
            Some(i) => (i.done.clone(), i.late.clone(), stale),
            None => {
                let (tx, rx) = watch::channel(false);
                let late = Arc::new(AtomicBool::new(false));
                s.inflight = Some(Inflight {
                    key: key.clone(),
                    done: rx.clone(),
                    late: late.clone(),
                });
                let (key2, late2) = (key.clone(), late.clone());
                tauri::async_runtime::spawn(async move {
                    let result = fetch_all(&account, &cals, today).await;
                    if let Ok(mut s) = state().lock() {
                        s.entry = Some(Entry {
                            key: key2.clone(),
                            date: today,
                            at: Instant::now(),
                            result,
                        });
                        if s.inflight.as_ref().is_some_and(|i| i.key == key2) {
                            s.inflight = None;
                        }
                    }
                    let _ = tx.send(true);
                    if late2.load(Ordering::SeqCst) {
                        if let Some(app) = app {
                            let _ = app.emit(super::CHANGED_EVENT, ());
                        }
                    }
                });
                (rx, late, stale)
            }
        }
    };

    if tokio::time::timeout(WAIT, done.wait_for(|d| *d)).await.is_ok() {
        if let Some(e) = state().lock().ok().and_then(|s| s.entry.clone()).filter(|e| e.key == key) {
            return e.result;
        }
    }
    late.store(true, Ordering::SeqCst);
    stale.ok_or_else(|| ERR_LOADING.to_string())
}

/// Local wall-clock events (unparseable ones dropped).
pub fn to_local(events: &[BusyEvent]) -> Vec<CalEvent> {
    let parse = |s: &str, all_day: bool| -> Option<NaiveDateTime> {
        if all_day {
            NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?.and_hms_opt(0, 0, 0)
        } else {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&Local).naive_local())
        }
    };
    events
        .iter()
        .filter_map(|e| {
            Some(CalEvent {
                start: parse(&e.start, e.all_day)?,
                end: parse(&e.end, e.all_day)?,
                title: e.title.clone(),
                all_day: e.all_day,
                transparent: e.transparent,
                declined: e.declined,
            })
        })
        .collect()
}
