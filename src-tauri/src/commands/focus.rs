//! Focus board commands (docs/focus-board-spec.md §6.1, phase P1). All the
//! ranking happens here (`focus::score`); the frontend only renders.

use crate::commands::todos::{load_todos, Todo};
use crate::db::Db;
use crate::focus::score::{self, Bucket, Calibration, Reason, TodoInput, WorkHours};
use crate::focus::capacity::{self, BusyOut, CalEvent, Capacity};
use crate::focus::settings::{self as focus_settings, FocusSettings};
use chrono::{DateTime, NaiveDateTime, TimeZone};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Serialize)]
pub struct BoardItem {
    pub todo: Todo,
    pub bucket: Bucket,
    pub score: i64,
    pub reasons: Vec<Reason>,
    /// RFC3339, local offset.
    pub start_by: Option<String>,
    /// RFC3339, local offset.
    pub effective_due: Option<String>,
    pub effective_estimate_min: i64,
    pub estimate_missing: bool,
}

#[derive(Debug, Serialize)]
pub struct CalibrationInfo {
    /// Applied to estimates (1.0 when calibration is off).
    pub factor: f64,
    pub samples: usize,
    pub enabled: bool,
    /// The factor computed from history, whether applied or not.
    pub measured: f64,
}

#[derive(Debug, Serialize)]
pub struct Board {
    /// RFC3339, local offset.
    pub now: String,
    /// Bucket display order, then rank inside each bucket (done: newest first).
    pub items: Vec<BoardItem>,
    /// "Do now" todo ids, at most 3.
    pub top: Vec<String>,
    /// Today's workload against the Calendar (BR-08); null when unavailable.
    pub capacity: Option<Capacity>,
    /// Why `capacity` is null: `no_account` (no Google account linked),
    /// `loading` (first fetch still running; `focus:changed` follows), or the
    /// Calendar error.
    pub capacity_error: Option<String>,
    pub calibration: CalibrationInfo,
}

pub(crate) fn to_local<Tz: TimeZone>(s: Option<&str>, tz: &Tz) -> Option<NaiveDateTime> {
    DateTime::parse_from_rfc3339(s?.trim())
        .ok()
        .map(|d| d.with_timezone(tz).naive_local())
}

fn to_rfc<Tz: TimeZone>(n: NaiveDateTime, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    tz.from_local_datetime(&n)
        .earliest()
        .map(|d| d.to_rfc3339())
        // A wall-clock time skipped by a DST jump: read it an hour later.
        .or_else(|| {
            tz.from_local_datetime(&(n + chrono::Duration::hours(1)))
                .earliest()
                .map(|d| d.to_rfc3339())
        })
        .unwrap_or_else(|| n.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Ranked todos at one instant (pure): rows in display order (done ones
/// completed in the last 7 days only) and the "do now" ids.
pub struct Ranking {
    pub rows: Vec<(Todo, score::Ranked)>,
    pub top: Vec<String>,
    pub calibration: CalibrationInfo,
}

pub fn rank_todos<Tz: TimeZone>(
    todos: Vec<Todo>,
    now: &DateTime<Tz>,
    wh: &WorkHours,
    calibration_enabled: bool,
    samples: &[(i64, i64)],
) -> Ranking {
    let tz = now.timezone();
    let now_local = now.naive_local();
    let Calibration { factor: measured, samples: n } = score::calibration(samples);
    let factor = if calibration_enabled { measured } else { 1.0 };

    let mut ranked: Vec<(Todo, score::Ranked, Option<NaiveDateTime>)> = todos
        .into_iter()
        .filter_map(|t| {
            let completed = to_local(t.completed_at.as_deref(), &tz);
            if t.done && !score::done_visible(completed, now_local) {
                return None;
            }
            let input = TodoInput {
                done: t.done,
                status: t.status.clone(),
                priority: t.priority,
                estimate_min: t.estimate_min,
                due_date: t.due_date.clone(),
                due_time: t.due_time.clone(),
                created_at: to_local(Some(&t.created_at), &tz),
                snoozed_until: to_local(t.snoozed_until.as_deref(), &tz),
            };
            let r = score::rank(&input, now_local, wh, factor);
            Some((t, r, completed))
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.1.bucket.cmp(&b.1.bucket).then_with(|| {
            if a.1.bucket == Bucket::Done {
                b.2.cmp(&a.2)
            } else {
                score::cmp_rank((&a.1, a.0.position), (&b.1, b.0.position))
            }
        })
    });
    let pairs: Vec<(score::Ranked, i64)> =
        ranked.iter().map(|(t, r, _)| (r.clone(), t.position)).collect();
    let top = score::top(&pairs)
        .into_iter()
        .map(|i| ranked[i].0.id.clone())
        .collect();
    Ranking {
        rows: ranked.into_iter().map(|(t, r, _)| (t, r)).collect(),
        top,
        calibration: CalibrationInfo {
            factor,
            samples: n,
            enabled: calibration_enabled,
            measured,
        },
    }
}

/// The board for `todos` at `now` (pure; `samples` = `(actual, estimate)` of
/// done todos, newest first). `capacity` is filled by the caller.
pub fn build_board<Tz: TimeZone>(
    todos: Vec<Todo>,
    now: DateTime<Tz>,
    wh: &WorkHours,
    calibration_enabled: bool,
    samples: &[(i64, i64)],
) -> Board
where
    Tz::Offset: std::fmt::Display,
{
    let tz = now.timezone();
    let Ranking { rows, top, calibration } = rank_todos(todos, &now, wh, calibration_enabled, samples);
    let items = rows
        .into_iter()
        .map(|(todo, r)| BoardItem {
            todo,
            bucket: r.bucket,
            score: r.score,
            reasons: r.reasons,
            start_by: r.start_by.map(|d| to_rfc(d, &tz)),
            effective_due: r.effective_due.map(|d| to_rfc(d, &tz)),
            effective_estimate_min: r.effective_estimate_min,
            estimate_missing: r.estimate_missing,
        })
        .collect();
    Board {
        now: now.to_rfc3339(),
        items,
        top,
        capacity: None,
        capacity_error: None,
        calibration,
    }
}

/// BR-08 for the board at `now` with today's Calendar `events` (pure).
pub fn capacity_for<Tz: TimeZone>(
    board: &Board,
    now: &DateTime<Tz>,
    wh: &WorkHours,
    events: &[CalEvent],
) -> Capacity
where
    Tz::Offset: std::fmt::Display,
{
    let tz = now.timezone();
    let now_local = now.naive_local();
    let busy = capacity::busy_on(events, now_local.date());
    Capacity {
        need_min: capacity::need_minutes(
            board.items.iter().map(|i| (i.bucket, i.effective_estimate_min)),
        ),
        free_min: capacity::free_minutes(now_local, wh, &busy),
        busy: busy
            .into_iter()
            .map(|b| BusyOut {
                start: to_rfc(b.start, &tz),
                end: to_rfc(b.end, &tz),
                title: b.title,
            })
            .collect(),
    }
}

/// `(actual_min, estimate_min)` of the 20 most recently completed todos that
/// have both (BR-10).
async fn calibration_samples(db: &Db) -> Result<Vec<(i64, i64)>, String> {
    sqlx::query_as(
        "SELECT actual_min, estimate_min FROM todos \
         WHERE done = 1 AND actual_min > 0 AND estimate_min > 0 \
         ORDER BY completed_at DESC LIMIT 20",
    )
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())
}

/// Settings, todos and calibration samples of the board.
pub async fn board_inputs(db: &Db) -> Result<(FocusSettings, Vec<Todo>, Vec<(i64, i64)>), String> {
    Ok((
        focus_settings::load(db).await?,
        load_todos(db).await?,
        calibration_samples(db).await?,
    ))
}

#[tauri::command]
pub async fn focus_board(app: AppHandle, db: State<'_, Db>) -> Result<Board, String> {
    let db = db.inner();
    let (settings, todos, samples) = board_inputs(db).await?;
    let now = chrono::Local::now();
    let wh = settings.work_hours();
    let mut board = build_board(todos, now, &wh, settings.calibration_enabled, &samples);
    match crate::focus::calendar::today_events(
        db,
        &settings,
        Some(app),
        crate::focus::calendar::BOARD_WAIT,
    )
    .await {
        Ok(events) => {
            let events = crate::focus::calendar::to_local(&events);
            board.capacity = Some(capacity_for(&board, &now, &wh, &events));
        }
        Err(e) => board.capacity_error = Some(e),
    }
    Ok(board)
}

/// Drop the cached Calendar events; the next `focus_board` fetches again.
#[tauri::command]
pub async fn focus_refresh_calendar(app: AppHandle) -> Result<(), String> {
    crate::focus::calendar::invalidate();
    let _ = app.emit(crate::focus::CHANGED_EVENT, ());
    Ok(())
}

/// The todo a reminder click asked to open, once (the Focus window reads it
/// on mount in case it was created by the click).
#[tauri::command]
pub fn focus_take_open_todo() -> Option<String> {
    crate::focus::scheduler::take_pending_open()
}

#[tauri::command]
pub async fn focus_settings_get(db: State<'_, Db>) -> Result<FocusSettings, String> {
    focus_settings::load(db.inner()).await
}

/// Validate and save; returns the stored (normalised) settings.
#[tauri::command]
pub async fn focus_settings_set(
    app: AppHandle,
    db: State<'_, Db>,
    settings: FocusSettings,
) -> Result<FocusSettings, String> {
    let saved = focus_settings::save(db.inner(), settings).await?;
    crate::focus::emit_todos_changed(&app);
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(7 * 3600).unwrap()
    }

    fn todo(id: &str, pos: i64) -> Todo {
        Todo {
            id: id.into(),
            text: id.into(),
            done: false,
            position: pos,
            created_at: "2026-10-28T03:00:00Z".into(),
            project_id: None,
            due_date: None,
            due_time: None,
            priority: 2,
            estimate_min: None,
            estimate_source: None,
            status: "todo".into(),
            snoozed_until: None,
            remind_at: None,
            source_type: None,
            source_ref: None,
            source_url: None,
            started_at: None,
            completed_at: None,
            actual_min: None,
            ai_plan: None,
            ai_analyzed_at: None,
            updated_at: None,
            linked_run_count: 0,
        }
    }

    #[test]
    fn board_buckets_orders_and_formats() {
        // Wednesday 2026-10-28 11:00 +07:00.
        let now = tz().with_ymd_and_hms(2026, 10, 28, 11, 0, 0).unwrap();
        let mut overdue = todo("overdue", 3);
        overdue.due_date = Some("2026-10-27".into());
        let mut today = todo("today", 0);
        today.due_date = Some("2026-10-28".into());
        today.due_time = Some("15:00".into());
        today.estimate_min = Some(30);
        let mut done_new = todo("done_new", 1);
        done_new.done = true;
        done_new.completed_at = Some("2026-10-27T00:00:00Z".into());
        let mut done_old = todo("done_old", 2);
        done_old.done = true;
        done_old.completed_at = Some("2026-10-01T00:00:00Z".into());
        let mut done_legacy = todo("done_legacy", 4);
        done_legacy.done = true;
        let mut snoozed = todo("snoozed", 5);
        snoozed.snoozed_until = Some("2026-10-28T08:00:00Z".into()); // 15:00 local
        let plain = todo("plain", 6);

        let b = build_board(
            vec![plain, done_old, today, snoozed, done_legacy, overdue, done_new],
            now,
            &WorkHours::default(),
            true,
            &[],
        );
        let ids: Vec<(&str, Bucket)> =
            b.items.iter().map(|i| (i.todo.id.as_str(), i.bucket)).collect();
        assert_eq!(
            ids,
            vec![
                ("overdue", Bucket::Overdue),
                ("today", Bucket::Today),
                ("plain", Bucket::NoDue),
                ("snoozed", Bucket::Snoozed),
                ("done_new", Bucket::Done),
            ]
        );
        assert_eq!(b.top, vec!["overdue", "today", "plain"]);
        assert_eq!(b.now, "2026-10-28T11:00:00+07:00");
        let today = &b.items[1];
        assert_eq!(today.effective_due.as_deref(), Some("2026-10-28T15:00:00+07:00"));
        assert_eq!(today.start_by.as_deref(), Some("2026-10-28T14:15:00+07:00"));
        // 240 working minutes left − 30 estimate − 15 aging (created 10:00).
        assert_eq!(today.score, 240 - 30 - 15);
        assert!(b.items[2].estimate_missing);
        assert!(b.capacity.is_none());
        assert_eq!((b.calibration.factor, b.calibration.samples), (1.0, 0));
    }

    #[test]
    fn calibration_applies_only_when_enabled() {
        let now = tz().with_ymd_and_hms(2026, 10, 28, 11, 0, 0).unwrap();
        let mut t = todo("a", 0);
        t.estimate_min = Some(60);
        let samples = [(90, 60); 6];
        let on = build_board(vec![t.clone()], now, &WorkHours::default(), true, &samples);
        assert_eq!(on.calibration.factor, 1.5);
        assert_eq!(on.items[0].effective_estimate_min, 90);
        let off = build_board(vec![t], now, &WorkHours::default(), false, &samples);
        assert_eq!((off.calibration.factor, off.calibration.measured), (1.0, 1.5));
        assert_eq!(off.items[0].effective_estimate_min, 60);
    }

    #[test]
    fn capacity_from_the_board() {
        // Wednesday 2026-10-28 13:00 +07:00.
        let now = tz().with_ymd_and_hms(2026, 10, 28, 13, 0, 0).unwrap();
        let mut overdue = todo("overdue", 0);
        overdue.due_date = Some("2026-10-27".into());
        overdue.estimate_min = Some(30);
        let mut today = todo("today", 1);
        today.due_date = Some("2026-10-28".into());
        today.estimate_min = Some(120);
        let mut doing = todo("doing", 2);
        doing.status = "doing".into();
        let later = todo("later", 3);
        let wh = WorkHours::default();
        let board = build_board(vec![overdue, today, doing, later], now, &wh, true, &[]);
        let at = |h: u32, m: u32| chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap().and_hms_opt(h, m, 0).unwrap();
        let events = vec![
            CalEvent { start: at(14, 0), end: at(15, 0), title: "Sync".into(), all_day: false, transparent: false, declined: false },
            CalEvent { start: at(14, 30), end: at(16, 0), title: "Review".into(), all_day: false, transparent: false, declined: false },
        ];
        let c = capacity_for(&board, &now, &wh, &events);
        // need: 30 + 120 + 60 (doing, no estimate); free: 13:00–18:00 = 300 − 120.
        assert_eq!((c.need_min, c.free_min), (210, 180));
        assert_eq!(c.busy.len(), 2);
        assert_eq!(c.busy[0].start, "2026-10-28T14:00:00+07:00");
    }

}
