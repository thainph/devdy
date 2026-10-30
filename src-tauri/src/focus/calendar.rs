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
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

const TTL: Duration = Duration::from_secs(10 * 60);
/// How long `focus_board` waits on Google.
pub const BOARD_WAIT: Duration = Duration::from_secs(3);
/// How long "plan today" waits: planning over meetings is worse than waiting.
pub const PLAN_WAIT: Duration = Duration::from_secs(15);

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
    /// A caller stopped waiting: announce the result when it lands. Read and
    /// written under the state lock, so a fetch finishing right as a caller
    /// gives up is either returned to it or announced, never lost.
    late: bool,
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
/// `ERR_LOADING`, or the Google error), waiting at most `wait` on Google (a
/// slower fetch keeps going and fills the cache). `app` gets `focus:changed`
/// when a fetch outlives a caller's wait.
pub async fn today_events(
    db: &Db,
    settings: &FocusSettings,
    app: Option<AppHandle>,
    wait: Duration,
) -> Result<Vec<BusyEvent>, String> {
    let account = match settings.calendar_account_id.clone() {
        Some(a) => a,
        None => default_google_account(db).await.ok_or_else(|| ERR_NO_ACCOUNT.to_string())?,
    };
    let cals = calendars(settings);
    let today = Local::now().date_naive();
    let key = format!("{account}|{}|{today}", cals.join(","));
    let fetch = async move { fetch_all(&account, &cals, today).await };
    cached_or_fetch(key, today, fetch, app, wait).await
}

/// The cache / single-flight / budget logic of `today_events`, over any fetch
/// (a fake in tests).
async fn cached_or_fetch<F>(
    key: String,
    today: NaiveDate,
    fetch: F,
    app: Option<AppHandle>,
    wait: Duration,
) -> Result<Vec<BusyEvent>, String>
where
    F: std::future::Future<Output = Result<Vec<BusyEvent>, String>> + Send + 'static,
{
    let mut done = {
        let mut s = state().lock().map_err(|e| e.to_string())?;
        if let Some(e) = s.entry.as_ref().filter(|e| e.key == key && e.at.elapsed() < TTL) {
            return e.result.clone();
        }
        match s.inflight.as_ref().filter(|i| i.key == key) {
            Some(i) => i.done.clone(),
            None => {
                let (tx, rx) = watch::channel(false);
                s.inflight = Some(Inflight {
                    key: key.clone(),
                    done: rx.clone(),
                    late: false,
                });
                let key2 = key.clone();
                tauri::async_runtime::spawn(async move {
                    let result = fetch.await;
                    let late = match state().lock() {
                        Ok(mut s) => {
                            s.entry = Some(Entry {
                                key: key2.clone(),
                                date: today,
                                at: Instant::now(),
                                result,
                            });
                            match s.inflight.take() {
                                Some(i) if i.key == key2 => i.late,
                                other => {
                                    s.inflight = other;
                                    false
                                }
                            }
                        }
                        Err(_) => false,
                    };
                    let _ = tx.send(true);
                    if late {
                        if let Some(app) = app {
                            let _ = app.emit(super::CHANGED_EVENT, ());
                        }
                    }
                });
                rx
            }
        }
    };

    let _ = tokio::time::timeout(wait, done.wait_for(|d| *d)).await;
    let mut s = state().lock().map_err(|e| e.to_string())?;
    if let Some(i) = s.inflight.as_mut().filter(|i| i.key == key) {
        // Still running: it will announce itself; meanwhile today's last result.
        i.late = true;
        return s
            .entry
            .as_ref()
            .filter(|e| e.date == today && e.key == key)
            .and_then(|e| e.result.clone().ok())
            .ok_or_else(|| ERR_LOADING.to_string());
    }
    match s.entry.as_ref().filter(|e| e.key == key) {
        Some(e) => e.result.clone(),
        None => Err(ERR_LOADING.to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is one global slot: these tests run one at a time.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn ev(title: &str) -> BusyEvent {
        BusyEvent {
            start: "2026-10-28T10:00:00+07:00".into(),
            end: "2026-10-28T11:00:00+07:00".into(),
            all_day: false,
            title: title.into(),
            transparent: false,
            declined: false,
        }
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()
    }

    #[tokio::test]
    async fn slow_fetch_reports_loading_then_lands_in_the_cache() {
        let _g = SERIAL.lock().await;
        invalidate();
        let slow = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(vec![ev("Standup")])
        };
        let r = cached_or_fetch("slow".into(), day(), slow, None, Duration::from_millis(20)).await;
        assert_eq!(r.unwrap_err(), ERR_LOADING);
        // A second caller joins the same flight (no new fetch) and, with a
        // longer budget, gets the result.
        let never = async { Err::<Vec<BusyEvent>, String>("second fetch".into()) };
        let r = cached_or_fetch("slow".into(), day(), never, None, Duration::from_secs(5)).await;
        assert_eq!(r.unwrap()[0].title, "Standup");
        assert!(state().lock().unwrap().inflight.is_none());
    }

    #[tokio::test]
    async fn errors_are_cached_until_invalidated() {
        let _g = SERIAL.lock().await;
        invalidate();
        let fail = async { Err::<Vec<BusyEvent>, String>("events rejected".into()) };
        let r = cached_or_fetch("err".into(), day(), fail, None, Duration::from_secs(5)).await;
        assert_eq!(r.unwrap_err(), "events rejected");
        let ok = async { Ok(vec![ev("x")]) };
        let r = cached_or_fetch("err".into(), day(), ok, None, Duration::from_secs(5)).await;
        assert_eq!(r.unwrap_err(), "events rejected", "cached");
        invalidate();
        let ok = async { Ok(vec![ev("x")]) };
        let r = cached_or_fetch("err".into(), day(), ok, None, Duration::from_secs(5)).await;
        assert_eq!(r.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_timed_out_caller_marks_the_flight_late() {
        let _g = SERIAL.lock().await;
        invalidate();
        let slow = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(vec![])
        };
        let _ = cached_or_fetch("late".into(), day(), slow, None, Duration::from_millis(10)).await;
        assert!(state().lock().unwrap().inflight.as_ref().is_some_and(|i| i.late));
        // Let it land before the next test uses the slot.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(state().lock().unwrap().inflight.is_none());
    }
}
