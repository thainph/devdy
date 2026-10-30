//! Focus reminders (docs/focus-board-spec.md §3.4, BR-07): a 60 s tick that
//! decides which notifications are due (`decide`, pure) and shows them as ONE
//! clickable macOS notification (`compose`, pure). The I/O wrapper (`start`)
//! reads the board, records what fired in `todo_notifications` (local, not
//! synced) and routes a click to the Focus window.
//!
//! Rules:
//! - `remind` (user-set `remind_at`) fires any time; every other kind only
//!   inside working hours on a working day.
//! - `morning`: once per working day, from `morning_time` until the end of
//!   work, when an open todo exists (keyed `_morning` / date; a missed one
//!   still fires later that day).
//! - `start_by`: `start_by <= now < due`, status `todo`, not snoozed.
//! - `due_soon`: todos with a due *time*, from 30 min before the due.
//! - `overdue`: once, right after the due passed.
//! - `snooze_end`: the snooze ended and the todo is in "do now".
//! - A notification whose moment is more than 2 h old is dropped (the machine
//!   slept through it), except `morning` on the same day.
//! - Dedupe key `(todo_id, kind, fire_key)`; `fire_key` is the instant that
//!   triggered it (effective due / `remind_at` / `snoozed_until` / date), so a
//!   moved deadline can remind again.

use super::score::{Bucket, WorkHours};
use super::settings::FocusSettings;
use chrono::{Duration, NaiveDateTime, NaiveTime};
use std::collections::HashSet;

/// Synthetic todo id of the morning summary row.
pub const MORNING_ID: &str = "_morning";
/// Older notifications than this are skipped (machine asleep).
pub const MISSED_AFTER_MIN: i64 = 120;
/// An `overdue` deadline on the edge of the working day still fires this long
/// after it passes, outside working hours.
pub const OVERDUE_GRACE_MIN: i64 = 15;
/// `due_soon` lead time.
pub const DUE_SOON_MIN: i64 = 30;
/// Lines listed in a grouped notification.
const MAX_LINES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Morning,
    StartBy,
    DueSoon,
    Overdue,
    Remind,
    SnoozeEnd,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Morning => "morning",
            Kind::StartBy => "start_by",
            Kind::DueSoon => "due_soon",
            Kind::Overdue => "overdue",
            Kind::Remind => "remind",
            Kind::SnoozeEnd => "snooze_end",
        }
    }

    fn enabled(self, s: &FocusSettings) -> bool {
        let k = &s.notify_kinds;
        match self {
            Kind::Morning => k.morning,
            Kind::StartBy => k.start_by,
            Kind::DueSoon => k.due_soon,
            Kind::Overdue => k.overdue,
            Kind::Remind => k.remind,
            Kind::SnoozeEnd => k.snooze_end,
        }
    }
}

/// What the scheduler reads of a ranked todo (local wall-clock times).
#[derive(Debug, Clone, PartialEq)]
pub struct SchedItem {
    pub id: String,
    pub text: String,
    pub status: String,
    pub bucket: Bucket,
    pub effective_due: Option<NaiveDateTime>,
    pub has_due_time: bool,
    pub start_by: Option<NaiveDateTime>,
    pub estimate_min: i64,
    pub snoozed_until: Option<NaiveDateTime>,
    pub remind_at: Option<NaiveDateTime>,
    /// In "do now" (the board's top 3).
    pub in_top: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub kind: Kind,
    /// `MORNING_ID` for the morning summary.
    pub todo_id: String,
    pub fire_key: String,
    /// One localised line ("Nên bắt đầu: … (~2h, hạn 17:00)").
    pub line: String,
}

/// `(todo_id, kind, fire_key)` already shown on this machine.
pub type Fired = HashSet<(String, String, String)>;

fn key(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M").to_string()
}

fn vi(lang: &str) -> bool {
    lang.starts_with("vi")
}

fn hm(min: i64) -> String {
    let (h, m) = (min / 60, min % 60);
    match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m:02}"),
    }
}

fn due_label(due: NaiveDateTime, now: NaiveDateTime) -> String {
    if due.date() == now.date() {
        due.format("%H:%M").to_string()
    } else {
        due.format("%d/%m %H:%M").to_string()
    }
}

fn line(kind: Kind, it: &SchedItem, now: NaiveDateTime, lang: &str) -> String {
    let t = it.text.lines().next().unwrap_or("").trim();
    let t: String = if t.chars().count() > 80 {
        format!("{}…", t.chars().take(79).collect::<String>())
    } else {
        t.to_string()
    };
    let v = vi(lang);
    match kind {
        Kind::StartBy => {
            let due = it.effective_due.map(|d| due_label(d, now)).unwrap_or_default();
            if v {
                format!("Nên bắt đầu: {t} (~{}, hạn {due})", hm(it.estimate_min))
            } else {
                format!("Time to start: {t} (~{}, due {due})", hm(it.estimate_min))
            }
        }
        Kind::DueSoon => {
            if v {
                format!("Còn {DUE_SOON_MIN} phút: {t}")
            } else {
                format!("{DUE_SOON_MIN} min left: {t}")
            }
        }
        Kind::Overdue => {
            if v {
                format!("Đã quá hạn: {t}")
            } else {
                format!("Overdue: {t}")
            }
        }
        Kind::Remind => {
            if v {
                format!("Nhắc: {t}")
            } else {
                format!("Reminder: {t}")
            }
        }
        Kind::SnoozeEnd => {
            if v {
                format!("Quay lại: {t}")
            } else {
                format!("Back on: {t}")
            }
        }
        Kind::Morning => String::new(),
    }
}

fn is_workday(d: chrono::NaiveDate, wh: &WorkHours) -> bool {
    use chrono::Datelike;
    wh.days[d.weekday().num_days_from_monday() as usize]
}

/// Whether `now` is inside the working window of a working day.
pub fn in_work_hours(now: NaiveDateTime, wh: &WorkHours) -> bool {
    is_workday(now.date(), wh) && now.time() >= wh.start && now.time() < wh.end
}

/// `t` itself inside working hours, else the start of the next working window
/// (`None` without any working day).
pub fn next_work_start(t: NaiveDateTime, wh: &WorkHours) -> Option<NaiveDateTime> {
    if in_work_hours(t, wh) {
        return Some(t);
    }
    (0..=14)
        .map(|i| t.date() + Duration::days(i))
        .filter(|d| is_workday(*d, wh))
        .map(|d| d.and_time(wh.start))
        .find(|ws| *ws >= t)
}

/// `overdue` may fire at `now` for a deadline `due <= now`:
/// - right when it passes, even just outside working hours, if the deadline
///   is on a working day within `[start, end]` (a date-only deadline is the
///   end of work) — up to `OVERDUE_GRACE_MIN` late;
/// - otherwise inside working hours, until 2 h after the first working moment
///   at / after the deadline (an evening / weekend deadline is reported once
///   on the next working morning).
fn overdue_due_now(due: NaiveDateTime, now: NaiveDateTime, wh: &WorkHours) -> bool {
    if due > now {
        return false;
    }
    let at_boundary = is_workday(due.date(), wh) && due.time() >= wh.start && due.time() <= wh.end;
    if at_boundary && now - due <= Duration::minutes(OVERDUE_GRACE_MIN) {
        return true;
    }
    in_work_hours(now, wh)
        && next_work_start(due, wh)
            .is_some_and(|from| now >= from && now - from <= Duration::minutes(MISSED_AFTER_MIN))
}

/// The notifications due at `now` (BR-07). `items` are the board's open
/// todos (done ones are ignored); `lang` is the app language (`en` / `vi`).
pub fn decide(
    now: NaiveDateTime,
    settings: &FocusSettings,
    items: &[SchedItem],
    fired: &Fired,
    lang: &str,
) -> Vec<Notice> {
    if !settings.notify_enabled {
        return Vec::new();
    }
    let wh = settings.work_hours();
    let working = in_work_hours(now, &wh);
    let missed = |at: NaiveDateTime| now - at > Duration::minutes(MISSED_AFTER_MIN);
    let mut out: Vec<Notice> = Vec::new();
    let mut push = |kind: Kind, todo_id: &str, fire_key: String, line: String| {
        let k = (todo_id.to_string(), kind.as_str().to_string(), fire_key.clone());
        if kind.enabled(settings) && !fired.contains(&k) {
            out.push(Notice {
                kind,
                todo_id: todo_id.to_string(),
                fire_key,
                line,
            });
        }
    };

    let open: Vec<&SchedItem> = items.iter().filter(|i| i.bucket != Bucket::Done).collect();

    if working {
        let morning_at = NaiveTime::parse_from_str(&settings.morning_time, "%H:%M")
            .map(|t| now.date().and_time(t))
            .ok();
        if let Some(at) = morning_at.filter(|at| *at <= now) {
            if !open.is_empty() {
                let n = open
                    .iter()
                    .filter(|i| matches!(i.bucket, Bucket::Doing | Bucket::Overdue | Bucket::Today))
                    .count();
                let m = open.iter().filter(|i| i.bucket == Bucket::Overdue).count();
                let first = items
                    .iter()
                    .find(|i| i.in_top)
                    .map(|i| i.text.lines().next().unwrap_or("").trim().to_string());
                let body = match (vi(lang), first) {
                    (true, Some(f)) => format!("Hôm nay {n} việc · {m} quá hạn · bắt đầu với: {f}"),
                    (true, None) => format!("Hôm nay {n} việc · {m} quá hạn"),
                    (false, Some(f)) => format!("Today: {n} tasks · {m} overdue · start with: {f}"),
                    (false, None) => format!("Today: {n} tasks · {m} overdue"),
                };
                push(Kind::Morning, MORNING_ID, at.date().format("%Y-%m-%d").to_string(), body);
            }
        }
    }

    for it in &open {
        if let Some(r) = it.remind_at.filter(|r| *r <= now && !missed(*r)) {
            push(Kind::Remind, &it.id, key(r), line(Kind::Remind, it, now, lang));
        }
        let snoozed = it.bucket == Bucket::Snoozed;
        if let Some(due) = it.effective_due.filter(|d| !snoozed && *d <= now) {
            if overdue_due_now(due, now, &wh) {
                push(Kind::Overdue, &it.id, key(due), line(Kind::Overdue, it, now, lang));
            }
        }
        if !working {
            continue;
        }
        if let Some(due) = it.effective_due.filter(|_| !snoozed) {
            let fk = key(due);
            if due > now {
                let soon = due - Duration::minutes(DUE_SOON_MIN);
                if it.has_due_time && soon <= now && !missed(soon) {
                    push(Kind::DueSoon, &it.id, fk.clone(), line(Kind::DueSoon, it, now, lang));
                }
                let start_now = it.start_by.is_some_and(|sb| sb <= now && !missed(sb));
                if start_now && it.status == "todo" {
                    push(Kind::StartBy, &it.id, fk, line(Kind::StartBy, it, now, lang));
                }
            }
        }
        if let Some(s) = it.snoozed_until.filter(|s| *s <= now && !missed(*s)) {
            if it.in_top {
                push(Kind::SnoozeEnd, &it.id, key(s), line(Kind::SnoozeEnd, it, now, lang));
            }
        }
    }
    out
}

/// One notification for a tick's notices: `(title, body, todo to open)`.
/// A single todo notice opens that todo; the morning summary or a group
/// opens the board (the first todo is still highlighted for a group).
pub fn compose(notices: &[Notice], lang: &str) -> Option<(String, String, Option<String>)> {
    let first = notices.first()?;
    let todo_of = |n: &Notice| (n.todo_id != MORNING_ID).then(|| n.todo_id.clone());
    if notices.len() == 1 {
        let title = if first.kind == Kind::Morning {
            if vi(lang) { "Focus · Hôm nay" } else { "Focus · Today" }
        } else {
            "Focus"
        };
        return Some((title.to_string(), first.line.clone(), todo_of(first)));
    }
    let n = notices.len();
    let title = if vi(lang) {
        format!("Focus · {n} nhắc việc")
    } else {
        format!("Focus · {n} reminders")
    };
    // The morning summary leads; the rest in decision order.
    let mut sorted: Vec<&Notice> = notices.iter().collect();
    sorted.sort_by_key(|x| x.kind != Kind::Morning);
    let mut lines: Vec<String> = sorted.iter().take(MAX_LINES).map(|x| x.line.clone()).collect();
    if n > MAX_LINES {
        lines.push(if vi(lang) {
            format!("… và {} việc khác", n - MAX_LINES)
        } else {
            format!("… and {} more", n - MAX_LINES)
        });
    }
    let open = notices.iter().find_map(todo_of);
    Some((title, lines.join("\n"), open))
}

// ---- Runtime -------------------------------------------------------------

/// Sent to every window when a reminder asks to show a todo: `{ todo_id }`.
pub const OPEN_TODO_EVENT: &str = "focus:open-todo";
/// Sent to the main window when the Focus window must be created (only the
/// frontend knows its URL / options, `src/lib/focusWindow.ts`).
pub const OPEN_WINDOW_EVENT: &str = "focus:open-window";
/// Label of the Focus window.
pub const FOCUS_WINDOW: &str = "focus";
const TICK: std::time::Duration = std::time::Duration::from_secs(60);
const FIRST_TICK: std::time::Duration = std::time::Duration::from_secs(10);
/// `todo_notifications` rows kept this long.
const KEEP_FIRED_DAYS: i64 = 30;

#[derive(Clone, serde::Serialize)]
struct OpenTodo {
    todo_id: Option<String>,
}

fn pending() -> &'static std::sync::Mutex<Option<String>> {
    static P: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// The todo the last reminder click asked to show, once.
pub fn take_pending_open() -> Option<String> {
    pending().lock().ok().and_then(|mut p| p.take())
}

/// Bring up the Focus window on `todo_id` (a reminder click, the tray row).
/// An open Focus window is raised and told directly; otherwise the main
/// window creates it (`focus:open-window`) and the new window picks the todo
/// up with `focus_take_open_todo` on mount.
pub fn open_focus(app: &tauri::AppHandle, todo_id: Option<String>) {
    use tauri::{Emitter, Manager};
    if let Ok(mut p) = pending().lock() {
        *p = todo_id.clone();
    }
    let payload = OpenTodo { todo_id };
    if let Some(win) = app.get_webview_window(FOCUS_WINDOW) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit(OPEN_TODO_EVENT, payload);
    } else {
        if let Some(main) = app.get_webview_window("main") {
            let _ = main.unminimize();
            let _ = main.show();
            let _ = main.set_focus();
        }
        let _ = app.emit_to("main", OPEN_WINDOW_EVENT, payload);
    }
}

async fn fired_set(db: &crate::db::Db) -> Result<Fired, String> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT todo_id, kind, fire_key FROM todo_notifications")
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().collect())
}

async fn record(db: &crate::db::Db, notices: &[Notice]) -> Result<(), String> {
    let now = chrono::Utc::now();
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;
    for n in notices {
        sqlx::query(
            "INSERT OR IGNORE INTO todo_notifications (todo_id, kind, fire_key, fired_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&n.todo_id)
        .bind(n.kind.as_str())
        .bind(&n.fire_key)
        .bind(now.to_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    sqlx::query("DELETE FROM todo_notifications WHERE fired_at < ?")
        .bind((now - Duration::days(KEEP_FIRED_DAYS)).to_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())
}

async fn language(db: &crate::db::Db) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'language'")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "en".into())
}

/// A cheap fingerprint of the `todos` table: row count, newest
/// `updated_at` / `created_at` and sums that move on (un)done / reorder.
/// Writes that bypass the app's commands (the MCP sidecar) show up as a
/// different value on the next tick.
pub async fn todos_signature(db: &crate::db::Db) -> Result<String, String> {
    sqlx::query_scalar(
        "SELECT COUNT(*) || '|' || COALESCE(MAX(updated_at), '') || '|' || \
                COALESCE(MAX(created_at), '') || '|' || COALESCE(SUM(done), 0) || '|' || \
                COALESCE(SUM(position), 0) FROM todos",
    )
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())
}

fn last_signature() -> &'static std::sync::Mutex<Option<String>> {
    static S: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Whether the table changed since the previous call (false on the first).
pub fn signature_changed(sig: String) -> bool {
    let Ok(mut last) = last_signature().lock() else { return false };
    let changed = last.as_ref().is_some_and(|l| *l != sig);
    *last = Some(sig);
    changed
}

/// One reminder pass: refresh the boards after outside writes, decide,
/// record, notify.
pub async fn tick(app: &tauri::AppHandle, db: &crate::db::Db) -> Result<usize, String> {
    use crate::commands::focus::{board_inputs, rank_todos, to_local};
    if signature_changed(todos_signature(db).await?) {
        super::emit_todos_changed(app);
    }
    let (settings, todos, samples) = board_inputs(db).await?;
    let now = chrono::Local::now();
    let tz = now.timezone();
    let wh = settings.work_hours();
    let ranking = rank_todos(todos, &now, &wh, settings.calibration_enabled, &samples);
    if !settings.notify_enabled {
        return Ok(0);
    }
    let items: Vec<SchedItem> = ranking
        .rows
        .iter()
        .map(|(t, r)| SchedItem {
            id: t.id.clone(),
            text: t.text.clone(),
            status: t.status.clone(),
            bucket: r.bucket,
            effective_due: r.effective_due,
            has_due_time: t.due_time.is_some(),
            start_by: r.start_by,
            estimate_min: r.effective_estimate_min,
            snoozed_until: to_local(t.snoozed_until.as_deref(), &tz),
            remind_at: to_local(t.remind_at.as_deref(), &tz),
            in_top: ranking.top.contains(&t.id),
        })
        .collect();
    let lang = language(db).await;
    let notices = decide(now.naive_local(), &settings, &items, &fired_set(db).await?, &lang);
    let Some((title, body, open)) = compose(&notices, &lang) else {
        return Ok(0);
    };
    // Recorded first: a crash mid-notify must not repeat it every minute.
    record(db, &notices).await?;
    crate::commands::notifications::show_clickable_opts(
        app.clone(),
        title,
        body,
        "focus reminder",
        false,
        move |app| open_focus(app, open),
    );
    Ok(notices.len())
}

/// Start the 60 s reminder loop (app setup). Runs while the app runs, with
/// or without a Focus window.
pub fn start(app: tauri::AppHandle, db: crate::db::Db) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_TICK).await;
        loop {
            if let Err(e) = tick(&app, &db).await {
                tracing::warn!(event = "focus_reminder_tick_failed", error = %e);
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-28 is a Wednesday; default hours 09:00–18:00, Mon–Fri.
    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn settings() -> FocusSettings {
        FocusSettings::default()
    }

    fn item(id: &str) -> SchedItem {
        SchedItem {
            id: id.into(),
            text: format!("Task {id}"),
            status: "todo".into(),
            bucket: Bucket::Today,
            effective_due: None,
            has_due_time: false,
            start_by: None,
            estimate_min: 60,
            snoozed_until: None,
            remind_at: None,
            in_top: false,
        }
    }

    fn kinds(n: &[Notice]) -> Vec<(&str, &str)> {
        n.iter().map(|x| (x.kind.as_str(), x.todo_id.as_str())).collect()
    }

    fn fired(n: &[Notice]) -> Fired {
        n.iter()
            .map(|x| (x.todo_id.clone(), x.kind.as_str().to_string(), x.fire_key.clone()))
            .collect()
    }

    /// Settings with the morning summary off (most tests look at one kind).
    fn quiet() -> FocusSettings {
        let mut s = settings();
        s.notify_kinds.morning = false;
        s
    }

    #[test]
    fn morning_once_per_working_day_and_late_same_day() {
        let items = vec![
            SchedItem { bucket: Bucket::Overdue, in_top: true, ..item("a") },
            SchedItem { bucket: Bucket::Later, ..item("b") },
        ];
        // Before the morning time: nothing.
        assert!(decide(at("2026-10-28 08:59"), &settings(), &items, &Fired::new(), "vi").is_empty());
        let n = decide(at("2026-10-28 09:00"), &settings(), &items, &Fired::new(), "vi");
        assert_eq!(kinds(&n), vec![("morning", MORNING_ID)]);
        assert_eq!(n[0].fire_key, "2026-10-28");
        assert_eq!(n[0].line, "Hôm nay 1 việc · 1 quá hạn · bắt đầu với: Task a");
        // Fired: not again today.
        assert!(decide(at("2026-10-28 10:00"), &settings(), &items, &fired(&n), "vi").is_empty());
        // Missed by the sleeping machine: still sent later that day (> 2 h).
        let late = decide(at("2026-10-28 15:00"), &settings(), &items, &Fired::new(), "en");
        assert_eq!(late[0].line, "Today: 1 tasks · 1 overdue · start with: Task a");
        // Weekend / after hours: no morning. No open todo: no morning.
        assert!(decide(at("2026-10-31 10:00"), &settings(), &items, &Fired::new(), "vi").is_empty());
        assert!(decide(at("2026-10-28 18:30"), &settings(), &items, &Fired::new(), "vi").is_empty());
        let done = vec![SchedItem { bucket: Bucket::Done, ..item("a") }];
        assert!(decide(at("2026-10-28 10:00"), &settings(), &done, &Fired::new(), "vi").is_empty());
    }

    #[test]
    fn start_by_due_soon_and_overdue() {
        let it = SchedItem {
            effective_due: Some(at("2026-10-28 17:00")),
            has_due_time: true,
            start_by: Some(at("2026-10-28 15:45")),
            estimate_min: 60,
            ..item("a")
        };
        let s = quiet();
        assert!(decide(at("2026-10-28 15:44"), &s, std::slice::from_ref(&it), &Fired::new(), "vi").is_empty());
        let n = decide(at("2026-10-28 15:45"), &s, std::slice::from_ref(&it), &Fired::new(), "vi");
        assert_eq!(kinds(&n), vec![("start_by", "a")]);
        assert_eq!(n[0].line, "Nên bắt đầu: Task a (~1h, hạn 17:00)");
        assert_eq!(n[0].fire_key, "2026-10-28T17:00");
        let f = fired(&n);
        assert!(decide(at("2026-10-28 16:00"), &s, std::slice::from_ref(&it), &f, "vi").is_empty());
        let n = decide(at("2026-10-28 16:30"), &s, std::slice::from_ref(&it), &f, "en");
        assert_eq!(kinds(&n), vec![("due_soon", "a")]);
        assert_eq!(n[0].line, "30 min left: Task a");
        let overdue = decide(at("2026-10-28 17:00"), &s, std::slice::from_ref(&it), &f, "en");
        assert_eq!(kinds(&overdue), vec![("overdue", "a")]);
        let f = fired(&overdue);
        assert!(decide(at("2026-10-28 17:10"), &s, std::slice::from_ref(&it), &f, "en").is_empty());
        // Overdue only "just after": skipped once more than 2 h late.
        assert!(decide(at("2026-10-28 19:01"), &s, std::slice::from_ref(&it), &Fired::new(), "en").is_empty());
        // A moved deadline is a new fire_key: may remind again.
        let moved = SchedItem { effective_due: Some(at("2026-10-28 17:30")), ..it };
        let n = decide(at("2026-10-28 17:30"), &s, &[moved], &f, "en");
        assert_eq!(kinds(&n), vec![("overdue", "a")]);
    }

    #[test]
    fn overdue_fires_for_date_only_and_off_hours_deadlines() {
        let s = quiet();
        // Date-only: due at the end of work (18:00).
        let it = SchedItem { effective_due: Some(at("2026-10-28 18:00")), ..item("a") };
        let tick = |now: &str, f: &Fired| decide(at(now), &s, std::slice::from_ref(&it), f, "en");
        assert!(tick("2026-10-28 17:59", &Fired::new()).is_empty());
        let n = tick("2026-10-28 18:00", &Fired::new());
        assert_eq!(kinds(&n), vec![("overdue", "a")]);
        let f = fired(&n);
        assert!(tick("2026-10-28 18:01", &f).is_empty());
        assert!(tick("2026-10-29 09:00", &f).is_empty(), "sent once");
        // Missed at 18:00 (machine asleep): late evening stays quiet, the
        // next working morning reports it once, within 2 h.
        assert_eq!(kinds(&tick("2026-10-28 18:01", &Fired::new())), vec![("overdue", "a")]);
        assert!(tick("2026-10-28 22:00", &Fired::new()).is_empty());
        assert_eq!(kinds(&tick("2026-10-29 09:00", &Fired::new())), vec![("overdue", "a")]);
        assert!(tick("2026-10-29 11:01", &Fired::new()).is_empty());
        // An evening deadline waits for the next working morning (Fri 20:00 → Mon).
        let eve = SchedItem { effective_due: Some(at("2026-10-30 20:00")), has_due_time: true, ..item("b") };
        let tick = |now: &str| decide(at(now), &s, std::slice::from_ref(&eve), &Fired::new(), "en");
        assert!(tick("2026-10-30 20:00").is_empty());
        assert!(tick("2026-10-31 10:00").is_empty());
        assert_eq!(kinds(&tick("2026-11-02 09:00")), vec![("overdue", "b")]);
        // Before work starts: reported at 09:00.
        let early = SchedItem { effective_due: Some(at("2026-10-28 08:00")), has_due_time: true, ..item("c") };
        let n = decide(at("2026-10-28 09:00"), &s, std::slice::from_ref(&early), &Fired::new(), "en");
        assert_eq!(kinds(&n), vec![("overdue", "c")]);
    }

    #[tokio::test]
    async fn todos_signature_moves_on_outside_writes() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let exec = |sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query(sql).execute(&db).await.unwrap() }
        };
        let empty = todos_signature(&db).await.unwrap();
        exec("INSERT INTO todos (id, text, done, position, created_at, updated_at) VALUES ('a', 'x', 0, 0, 'c1', 'u1')").await;
        let one = todos_signature(&db).await.unwrap();
        assert_ne!(empty, one);
        // MCP-style write: a newer updated_at.
        exec("UPDATE todos SET text = 'y', updated_at = 'u2' WHERE id = 'a'").await;
        let two = todos_signature(&db).await.unwrap();
        assert_ne!(one, two);
        // Synced row with an older updated_at but done flipped.
        exec("UPDATE todos SET done = 1, updated_at = 'u0' WHERE id = 'a'").await;
        assert_ne!(two, todos_signature(&db).await.unwrap());
        exec("DELETE FROM todos").await;
        assert_eq!(empty, todos_signature(&db).await.unwrap());
        // The tick refreshes only on a change after the first look.
        assert!(!signature_changed("s1".into()));
        assert!(!signature_changed("s1".into()));
        assert!(signature_changed("s2".into()));
    }

    #[test]
    fn next_work_start_skips_nights_and_weekends() {
        let wh = WorkHours::default();
        assert_eq!(next_work_start(at("2026-10-28 10:00"), &wh), Some(at("2026-10-28 10:00")));
        assert_eq!(next_work_start(at("2026-10-28 18:00"), &wh), Some(at("2026-10-29 09:00")));
        assert_eq!(next_work_start(at("2026-10-28 07:00"), &wh), Some(at("2026-10-28 09:00")));
        assert_eq!(next_work_start(at("2026-10-30 19:00"), &wh), Some(at("2026-11-02 09:00")));
    }

    #[test]
    fn due_soon_needs_a_time_and_start_by_needs_status_todo() {
        let s = quiet();
        let all_day = SchedItem {
            effective_due: Some(at("2026-10-28 18:00")),
            has_due_time: false,
            ..item("a")
        };
        assert!(decide(at("2026-10-28 17:40"), &s, &[all_day], &Fired::new(), "en").is_empty());
        let doing = SchedItem {
            status: "doing".into(),
            bucket: Bucket::Doing,
            effective_due: Some(at("2026-10-28 18:00")),
            start_by: Some(at("2026-10-28 16:45")),
            ..item("b")
        };
        assert!(decide(at("2026-10-28 17:00"), &s, &[doing], &Fired::new(), "en").is_empty());
    }

    #[test]
    fn snooze_blocks_due_kinds_and_snooze_end_needs_top() {
        let s = quiet();
        let snoozed = SchedItem {
            bucket: Bucket::Snoozed,
            effective_due: Some(at("2026-10-28 10:00")),
            snoozed_until: Some(at("2026-10-28 14:00")),
            ..item("a")
        };
        assert!(decide(at("2026-10-28 11:00"), &s, &[snoozed], &Fired::new(), "en").is_empty());
        let back = SchedItem {
            bucket: Bucket::Today,
            snoozed_until: Some(at("2026-10-28 14:00")),
            in_top: true,
            ..item("a")
        };
        let n = decide(at("2026-10-28 14:01"), &s, std::slice::from_ref(&back), &Fired::new(), "vi");
        assert_eq!(kinds(&n), vec![("snooze_end", "a")]);
        assert_eq!(n[0].line, "Quay lại: Task a");
        let not_top = SchedItem { in_top: false, ..back };
        assert!(decide(at("2026-10-28 14:01"), &s, &[not_top], &Fired::new(), "vi").is_empty());
    }

    #[test]
    fn outside_work_hours_only_remind() {
        let s = quiet();
        let it = SchedItem {
            effective_due: Some(at("2026-10-31 10:00")),
            has_due_time: true,
            start_by: Some(at("2026-10-31 08:00")),
            remind_at: Some(at("2026-10-31 09:50")),
            ..item("a")
        };
        // Saturday: overdue / due_soon / start_by are held back.
        let n = decide(at("2026-10-31 10:00"), &s, std::slice::from_ref(&it), &Fired::new(), "en");
        assert_eq!(kinds(&n), vec![("remind", "a")]);
        assert_eq!(n[0].fire_key, "2026-10-31T09:50");
        // A remind missed by more than 2 h is dropped.
        assert!(decide(at("2026-10-31 11:51"), &s, &[it], &Fired::new(), "en").is_empty());
    }

    #[test]
    fn master_switch_and_kind_switches() {
        let it = SchedItem { remind_at: Some(at("2026-10-28 10:00")), ..item("a") };
        let mut s = quiet();
        s.notify_enabled = false;
        assert!(decide(at("2026-10-28 10:00"), &s, std::slice::from_ref(&it), &Fired::new(), "en").is_empty());
        let mut s = quiet();
        s.notify_kinds.remind = false;
        assert!(decide(at("2026-10-28 10:00"), &s, &[it], &Fired::new(), "en").is_empty());
    }

    #[test]
    fn compose_groups_into_one_notification() {
        assert_eq!(compose(&[], "en"), None);
        let one = Notice {
            kind: Kind::Overdue,
            todo_id: "a".into(),
            fire_key: "k".into(),
            line: "Overdue: A".into(),
        };
        assert_eq!(
            compose(std::slice::from_ref(&one), "en"),
            Some(("Focus".into(), "Overdue: A".into(), Some("a".into())))
        );
        let morning = Notice {
            kind: Kind::Morning,
            todo_id: MORNING_ID.into(),
            fire_key: "d".into(),
            line: "Today: 2 tasks".into(),
        };
        assert_eq!(compose(std::slice::from_ref(&morning), "vi").unwrap().2, None);
        let many: Vec<Notice> = (0..6)
            .map(|i| Notice { todo_id: format!("t{i}"), line: format!("L{i}"), ..one.clone() })
            .chain([morning])
            .collect();
        let (title, body, open) = compose(&many, "vi").unwrap();
        assert_eq!(title, "Focus · 7 nhắc việc");
        assert_eq!(body, "Today: 2 tasks\nL0\nL1\nL2\n… và 3 việc khác");
        assert_eq!(open.as_deref(), Some("t0"));
    }
}
