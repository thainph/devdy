//! Focus ranking (spec BR-03..BR-06, BR-10), pure. All instants are local
//! wall-clock `NaiveDateTime`s; the command layer converts from / to RFC3339.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use serde::Serialize;
use serde_json::{json, Value};

/// Estimate assumed when a todo has none (flagged `estimate_missing`).
pub const DEFAULT_ESTIMATE_MIN: i64 = 60;
/// Buffer added before the due when computing `start_by` (BR-06).
pub const START_BY_BUFFER_MIN: i64 = 15;
/// Size of the "do now" list.
pub const TOP_N: usize = 3;
/// Done todos stay on the board this long.
pub const DONE_WINDOW_DAYS: i64 = 7;
/// Days scanned by the work-time walks (beyond that a due is "far").
const MAX_SCAN_DAYS: i64 = 3660;

/// Working hours (spec §2): `[start, end)` on `days`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkHours {
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// Index 0 = Monday … 6 = Sunday.
    pub days: [bool; 7],
}

impl Default for WorkHours {
    fn default() -> Self {
        WorkHours {
            start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            days: [true, true, true, true, true, false, false],
        }
    }
}

impl WorkHours {
    fn is_workday(&self, d: NaiveDate) -> bool {
        self.days[d.weekday().num_days_from_monday() as usize]
    }

    /// Minutes in one working day.
    pub fn day_minutes(&self) -> i64 {
        (self.end - self.start).num_minutes().max(0)
    }

    /// The working window of `d`, if it is a working day.
    fn window(&self, d: NaiveDate) -> Option<(NaiveDateTime, NaiveDateTime)> {
        (self.is_workday(d) && self.start < self.end)
            .then(|| (d.and_time(self.start), d.and_time(self.end)))
    }
}

/// Working minutes between two instants (0 when `to <= from`).
pub fn work_minutes(from: NaiveDateTime, to: NaiveDateTime, wh: &WorkHours) -> i64 {
    if to <= from {
        return 0;
    }
    let mut total = 0;
    let mut d = from.date();
    let last = to.date().min(from.date() + Duration::days(MAX_SCAN_DAYS));
    while d <= last {
        if let Some((ws, we)) = wh.window(d) {
            let s = ws.max(from);
            let e = we.min(to);
            if e > s {
                total += (e - s).num_minutes();
            }
        }
        d += Duration::days(1);
    }
    total
}

/// The instant `minutes` working minutes before `t`, walking back over the
/// working windows (`t` itself when `minutes <= 0`).
pub fn sub_work_minutes(t: NaiveDateTime, minutes: i64, wh: &WorkHours) -> NaiveDateTime {
    if minutes <= 0 || wh.day_minutes() == 0 || !wh.days.contains(&true) {
        return t;
    }
    let mut left = minutes;
    let mut d = t.date();
    for _ in 0..MAX_SCAN_DAYS {
        if let Some((ws, we)) = wh.window(d) {
            let e = we.min(t);
            if e > ws {
                let avail = (e - ws).num_minutes();
                if left <= avail {
                    return e - Duration::minutes(left);
                }
                left -= avail;
            }
        }
        d -= Duration::days(1);
    }
    t - Duration::minutes(minutes)
}

/// BR-03: `due_date + due_time`, or `due_date + end of work` without a time.
pub fn effective_due(
    due_date: Option<&str>,
    due_time: Option<&str>,
    wh: &WorkHours,
) -> Option<NaiveDateTime> {
    let date = NaiveDate::parse_from_str(due_date?.trim(), "%Y-%m-%d").ok()?;
    let time = due_time
        .and_then(|t| NaiveTime::parse_from_str(t.trim(), "%H:%M").ok())
        .unwrap_or(wh.end);
    Some(date.and_time(time))
}

/// BR-04 buckets, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    Doing,
    Overdue,
    Today,
    Tomorrow,
    ThisWeek,
    Later,
    NoDue,
    Waiting,
    Snoozed,
    Done,
}

/// One "why is it here" fact; the UI localises it from `kind` + `value`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reason {
    pub kind: &'static str,
    pub value: Value,
}

fn reason(kind: &'static str, value: Value) -> Reason {
    Reason { kind, value }
}

/// What the ranking reads from a todo.
#[derive(Debug, Clone, Default)]
pub struct TodoInput {
    pub done: bool,
    /// `todo | doing | waiting`.
    pub status: String,
    pub priority: i64,
    pub estimate_min: Option<i64>,
    pub due_date: Option<String>,
    pub due_time: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub snoozed_until: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ranked {
    pub bucket: Bucket,
    /// Lower = more urgent (minutes).
    pub score: i64,
    pub reasons: Vec<Reason>,
    pub start_by: Option<NaiveDateTime>,
    pub effective_due: Option<NaiveDateTime>,
    pub effective_estimate_min: i64,
    pub estimate_missing: bool,
}

/// Estimate × calibration factor (BR-10), default 60 min when missing.
pub fn effective_estimate(estimate_min: Option<i64>, factor: f64) -> i64 {
    let base = estimate_min.filter(|m| *m > 0).unwrap_or(DEFAULT_ESTIMATE_MIN);
    ((base as f64) * factor).round().max(1.0) as i64
}

pub fn priority_adj(priority: i64) -> i64 {
    match priority {
        1 => -480,
        3 => 480,
        _ => 0,
    }
}

/// `min(working minutes since creation / 4, 480)`: four working days old
/// ≈ one more working day of urgency (BR-05).
pub fn aging(created_at: Option<NaiveDateTime>, now: NaiveDateTime, wh: &WorkHours) -> i64 {
    created_at.map_or(0, |c| (work_minutes(c, now, wh) / 4).min(480))
}

/// Sunday of `d`'s (Monday-based) week.
fn week_end(d: NaiveDate) -> NaiveDate {
    d + Duration::days(6 - d.weekday().num_days_from_monday() as i64)
}

/// BR-04, without the done-window check (the caller filters old done todos).
pub fn bucket(t: &TodoInput, due: Option<NaiveDateTime>, now: NaiveDateTime) -> Bucket {
    if t.done {
        return Bucket::Done;
    }
    if t.snoozed_until.is_some_and(|s| s > now) {
        return Bucket::Snoozed;
    }
    match t.status.as_str() {
        "waiting" => return Bucket::Waiting,
        "doing" => return Bucket::Doing,
        _ => {}
    }
    let Some(due) = due else { return Bucket::NoDue };
    let today = now.date();
    if due < now {
        Bucket::Overdue
    } else if due.date() == today {
        Bucket::Today
    } else if due.date() == today + Duration::days(1) {
        Bucket::Tomorrow
    } else if due.date() <= week_end(today) {
        Bucket::ThisWeek
    } else {
        Bucket::Later
    }
}

/// BR-03..BR-06 for one todo.
pub fn rank(t: &TodoInput, now: NaiveDateTime, wh: &WorkHours, factor: f64) -> Ranked {
    let due = effective_due(t.due_date.as_deref(), t.due_time.as_deref(), wh);
    let est = effective_estimate(t.estimate_min, factor);
    let estimate_missing = t.estimate_min.is_none();
    let bucket = bucket(t, due, now);
    if bucket == Bucket::Done {
        return Ranked {
            bucket,
            score: 0,
            reasons: vec![reason("done", Value::Null)],
            start_by: None,
            effective_due: due,
            effective_estimate_min: est,
            estimate_missing,
        };
    }

    let mut reasons = Vec::new();
    match bucket {
        Bucket::Doing => reasons.push(reason("doing", Value::Null)),
        Bucket::Waiting => reasons.push(reason("waiting", Value::Null)),
        Bucket::Snoozed => reasons.push(reason(
            "snoozed",
            json!(t.snoozed_until.map(|s| s.format("%Y-%m-%dT%H:%M").to_string())),
        )),
        _ => {}
    }
    let slack = match due {
        Some(due) if due < now => {
            reasons.push(reason("overdue", json!((now - due).num_minutes())));
            -work_minutes(due, now, wh) - est
        }
        Some(due) => {
            let left = work_minutes(now, due, wh);
            reasons.push(reason("due", json!(due.format("%Y-%m-%dT%H:%M").to_string())));
            reasons.push(reason("work_left", json!(left)));
            if left < est {
                reasons.push(reason("late_by", json!(est - left)));
            }
            left - est
        }
        None => {
            reasons.push(reason("no_due", Value::Null));
            5 * wh.day_minutes()
        }
    };
    reasons.push(if estimate_missing {
        reason("estimate_missing", json!(est))
    } else {
        reason("estimate", json!(est))
    });
    if t.priority != 2 {
        reasons.push(reason("priority", json!(t.priority)));
    }
    let age = aging(t.created_at, now, wh);
    if let Some(c) = t.created_at.filter(|_| age > 0) {
        let days = (now - c).num_days();
        if days >= 2 {
            reasons.push(reason("age_days", json!(days)));
        }
    }
    Ranked {
        bucket,
        score: slack + priority_adj(t.priority) - age,
        reasons,
        start_by: due.map(|d| sub_work_minutes(d, est + START_BY_BUFFER_MIN, wh)),
        effective_due: due,
        effective_estimate_min: est,
        estimate_missing,
    }
}

/// Order inside a bucket / of the "do now" list: score, then earlier due,
/// then `position` (BR-05).
pub fn cmp_rank(a: (&Ranked, i64), b: (&Ranked, i64)) -> std::cmp::Ordering {
    let due = |r: &Ranked| r.effective_due.unwrap_or(NaiveDateTime::MAX);
    a.0.score
        .cmp(&b.0.score)
        .then_with(|| due(a.0).cmp(&due(b.0)))
        .then_with(|| a.1.cmp(&b.1))
}

/// Indexes of the "do now" todos: `doing` first, then by score; never
/// waiting / snoozed / done (BR-05, §3.2).
pub fn top(ranked: &[(Ranked, i64)]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..ranked.len())
        .filter(|&i| {
            !matches!(
                ranked[i].0.bucket,
                Bucket::Waiting | Bucket::Snoozed | Bucket::Done
            )
        })
        .collect();
    idx.sort_by(|&a, &b| {
        let doing = |i: usize| ranked[i].0.bucket != Bucket::Doing;
        doing(a).cmp(&doing(b)).then_with(|| {
            cmp_rank((&ranked[a].0, ranked[a].1), (&ranked[b].0, ranked[b].1))
        })
    });
    idx.truncate(TOP_N);
    idx
}

/// Whether a done todo still shows (completed in the last 7 days).
pub fn done_visible(completed_at: Option<NaiveDateTime>, now: NaiveDateTime) -> bool {
    completed_at.is_some_and(|c| c >= now - Duration::days(DONE_WINDOW_DAYS))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Calibration {
    pub factor: f64,
    pub samples: usize,
}

/// BR-10: median `actual / estimate` of the (up to) 20 most recent samples,
/// newest first; 1.0 under 5 samples; clamped to [0.5, 3.0].
pub fn calibration(samples: &[(i64, i64)]) -> Calibration {
    let mut ratios: Vec<f64> = samples
        .iter()
        .filter(|(actual, est)| *actual > 0 && *est > 0)
        .take(20)
        .map(|(actual, est)| *actual as f64 / *est as f64)
        .collect();
    let n = ratios.len();
    if n < 5 {
        return Calibration { factor: 1.0, samples: n };
    }
    ratios.sort_by(|a, b| a.total_cmp(b));
    let median = if n % 2 == 1 {
        ratios[n / 2]
    } else {
        (ratios[n / 2 - 1] + ratios[n / 2]) / 2.0
    };
    let factor = (median.clamp(0.5, 3.0) * 100.0).round() / 100.0;
    Calibration { factor, samples: n }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-28 is a Wednesday.
    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn wh() -> WorkHours {
        WorkHours::default()
    }

    fn todo() -> TodoInput {
        TodoInput {
            status: "todo".into(),
            priority: 2,
            ..Default::default()
        }
    }

    fn due(d: &str, t: Option<&str>) -> TodoInput {
        TodoInput {
            due_date: Some(d.into()),
            due_time: t.map(Into::into),
            ..todo()
        }
    }

    #[test]
    fn work_minutes_same_day_and_outside_hours() {
        assert_eq!(work_minutes(at("2026-10-28 10:00"), at("2026-10-28 12:30"), &wh()), 150);
        assert_eq!(work_minutes(at("2026-10-28 07:00"), at("2026-10-28 20:00"), &wh()), 540);
        assert_eq!(work_minutes(at("2026-10-28 19:00"), at("2026-10-28 23:00"), &wh()), 0);
        assert_eq!(work_minutes(at("2026-10-28 12:00"), at("2026-10-28 11:00"), &wh()), 0);
    }

    #[test]
    fn work_minutes_overnight_and_weekend() {
        // Wed 17:00 → Thu 10:00: 60 + 60.
        assert_eq!(work_minutes(at("2026-10-28 17:00"), at("2026-10-29 10:00"), &wh()), 120);
        // Fri 17:00 → Mon 10:00 skips the weekend.
        assert_eq!(work_minutes(at("2026-10-30 17:00"), at("2026-11-02 10:00"), &wh()), 120);
        // A whole weekend is worth nothing.
        assert_eq!(work_minutes(at("2026-10-31 08:00"), at("2026-11-01 20:00"), &wh()), 0);
        // Custom days: Saturday counts.
        let mut six = wh();
        six.days[5] = true;
        assert_eq!(work_minutes(at("2026-10-31 08:00"), at("2026-11-01 20:00"), &six), 540);
    }

    #[test]
    fn sub_work_minutes_walks_back_over_nights_and_weekends() {
        assert_eq!(sub_work_minutes(at("2026-10-28 15:00"), 75, &wh()), at("2026-10-28 13:45"));
        // Thu 10:00 − 2h → Wed 17:00.
        assert_eq!(sub_work_minutes(at("2026-10-29 10:00"), 120, &wh()), at("2026-10-28 17:00"));
        // Mon 09:30 − 1h → Fri 17:30.
        assert_eq!(sub_work_minutes(at("2026-11-02 09:30"), 60, &wh()), at("2026-10-30 17:30"));
        // A due outside hours starts from the end of that day's window.
        assert_eq!(sub_work_minutes(at("2026-10-28 22:00"), 30, &wh()), at("2026-10-28 17:30"));
        // Due on a Sunday: back to Friday.
        assert_eq!(sub_work_minutes(at("2026-11-01 18:00"), 60, &wh()), at("2026-10-30 17:00"));
        assert_eq!(sub_work_minutes(at("2026-10-28 15:00"), 0, &wh()), at("2026-10-28 15:00"));
    }

    #[test]
    fn effective_due_defaults_to_end_of_work() {
        assert_eq!(effective_due(Some("2026-10-28"), None, &wh()), Some(at("2026-10-28 18:00")));
        assert_eq!(effective_due(Some("2026-10-28"), Some("15:00"), &wh()), Some(at("2026-10-28 15:00")));
        // Weekend dues are kept as is.
        assert_eq!(effective_due(Some("2026-10-31"), None, &wh()), Some(at("2026-10-31 18:00")));
        assert_eq!(effective_due(None, Some("15:00"), &wh()), None);
        assert_eq!(effective_due(Some("garbage"), None, &wh()), None);
    }

    #[test]
    fn buckets_follow_br04() {
        let now = at("2026-10-28 11:00"); // Wednesday
        let b = |t: &TodoInput| {
            bucket(t, effective_due(t.due_date.as_deref(), t.due_time.as_deref(), &wh()), now)
        };
        assert_eq!(b(&due("2026-10-28", Some("10:00"))), Bucket::Overdue);
        assert_eq!(b(&due("2026-10-27", None)), Bucket::Overdue);
        assert_eq!(b(&due("2026-10-28", None)), Bucket::Today);
        assert_eq!(b(&due("2026-10-29", None)), Bucket::Tomorrow);
        assert_eq!(b(&due("2026-11-01", None)), Bucket::ThisWeek);
        assert_eq!(b(&due("2026-11-02", None)), Bucket::Later);
        assert_eq!(b(&todo()), Bucket::NoDue);
        // Precedence: done > snoozed > waiting > doing > time.
        let mut t = due("2026-10-20", None);
        t.status = "doing".into();
        assert_eq!(b(&t), Bucket::Doing);
        t.status = "waiting".into();
        assert_eq!(b(&t), Bucket::Waiting);
        t.snoozed_until = Some(at("2026-10-28 14:00"));
        assert_eq!(b(&t), Bucket::Snoozed);
        t.snoozed_until = Some(at("2026-10-28 10:00")); // snooze over
        assert_eq!(b(&t), Bucket::Waiting);
        t.done = true;
        assert_eq!(b(&t), Bucket::Done);
        // Sunday: tomorrow (Monday) beats "this week".
        let sun = at("2026-11-01 10:00");
        let t = due("2026-11-02", None);
        assert_eq!(bucket(&t, effective_due(t.due_date.as_deref(), None, &wh()), sun), Bucket::Tomorrow);
    }

    #[test]
    fn score_slack_due_today() {
        let now = at("2026-10-28 13:00");
        let mut t = due("2026-10-28", Some("15:00"));
        t.estimate_min = Some(30);
        let r = rank(&t, now, &wh(), 1.0);
        assert_eq!(r.bucket, Bucket::Today);
        assert_eq!(r.score, 120 - 30);
        assert_eq!(r.start_by, Some(at("2026-10-28 14:15")));
        assert!(!r.estimate_missing);
        let kinds: Vec<&str> = r.reasons.iter().map(|x| x.kind).collect();
        assert_eq!(kinds, vec!["due", "work_left", "estimate"]);
        assert_eq!(r.reasons[0].value, json!("2026-10-28T15:00"));
        assert_eq!(r.reasons[1].value, json!(120));
    }

    #[test]
    fn score_overnight_and_late() {
        // Due tomorrow 10:00, now 17:00: 120 working minutes, 3h needed.
        let mut t = due("2026-10-29", Some("10:00"));
        t.estimate_min = Some(180);
        let r = rank(&t, at("2026-10-28 17:00"), &wh(), 1.0);
        assert_eq!(r.score, 120 - 180);
        assert!(r.reasons.contains(&reason("late_by", json!(60))));
        // start_by = 10:00 − 195 working minutes → Wed 15:45.
        assert_eq!(r.start_by, Some(at("2026-10-28 15:45")));
    }

    #[test]
    fn score_overdue_is_negative_and_counts_work_time() {
        // Due Fri 18:00, now Mon 10:00: 60 working minutes late.
        let t = due("2026-10-30", None);
        let r = rank(&t, at("2026-11-02 10:00"), &wh(), 1.0);
        assert_eq!(r.bucket, Bucket::Overdue);
        assert_eq!(r.score, -60 - DEFAULT_ESTIMATE_MIN);
        assert!(r.estimate_missing);
        assert_eq!(r.reasons[0], reason("overdue", json!(64 * 60)));
        assert!(r.reasons.contains(&reason("estimate_missing", json!(60))));
    }

    #[test]
    fn score_no_due_priority_and_aging() {
        let now = at("2026-10-28 11:00");
        let r = rank(&todo(), now, &wh(), 1.0);
        assert_eq!(r.score, 5 * 540);
        assert_eq!(r.start_by, None);
        let mut p1 = todo();
        p1.priority = 1;
        assert_eq!(rank(&p1, now, &wh(), 1.0).score, 5 * 540 - 480);
        let mut p3 = todo();
        p3.priority = 3;
        assert_eq!(rank(&p3, now, &wh(), 1.0).score, 5 * 540 + 480);
        assert!(rank(&p3, now, &wh(), 1.0).reasons.contains(&reason("priority", json!(3))));
        // Created Mon 09:00, now Wed 11:00: 540*2 + 120 = 1200 working min → 300.
        let mut old = todo();
        old.created_at = Some(at("2026-10-26 09:00"));
        let r = rank(&old, now, &wh(), 1.0);
        assert_eq!(r.score, 5 * 540 - 300);
        assert!(r.reasons.contains(&reason("age_days", json!(2))));
        // Aging is capped at 480.
        old.created_at = Some(at("2026-09-01 09:00"));
        assert_eq!(rank(&old, now, &wh(), 1.0).score, 5 * 540 - 480);
    }

    #[test]
    fn calibration_scales_the_estimate() {
        let mut t = due("2026-10-28", Some("15:00"));
        t.estimate_min = Some(60);
        let r = rank(&t, at("2026-10-28 13:00"), &wh(), 1.5);
        assert_eq!(r.effective_estimate_min, 90);
        assert_eq!(r.score, 120 - 90);
    }

    #[test]
    fn done_and_window() {
        let now = at("2026-10-28 11:00");
        let mut t = due("2026-10-20", None);
        t.done = true;
        let r = rank(&t, now, &wh(), 1.0);
        assert_eq!((r.bucket, r.score, r.start_by), (Bucket::Done, 0, None));
        assert!(done_visible(Some(at("2026-10-21 12:00")), now));
        assert!(!done_visible(Some(at("2026-10-21 10:00")), now));
        assert!(!done_visible(None, now));
    }

    #[test]
    fn top_puts_doing_first_and_skips_parked() {
        let now = at("2026-10-28 11:00");
        let mk = |t: TodoInput, pos: i64| (rank(&t, now, &wh(), 1.0), pos);
        let mut doing = todo();
        doing.status = "doing".into();
        let mut waiting = due("2026-10-20", None);
        waiting.status = "waiting".into();
        let mut snoozed = due("2026-10-20", None);
        snoozed.snoozed_until = Some(at("2026-10-28 15:00"));
        let ranked = vec![
            mk(todo(), 0),                     // 0: no due
            mk(due("2026-10-27", None), 1),    // 1: overdue
            mk(waiting, 2),                    // 2
            mk(snoozed, 3),                    // 3
            mk(doing, 4),                      // 4: doing, no due
            mk(due("2026-10-28", None), 5),    // 5: today
        ];
        assert_eq!(top(&ranked), vec![4, 1, 5]);
    }

    #[test]
    fn ties_break_on_due_then_position() {
        let now = at("2026-10-28 11:00");
        let a = (rank(&todo(), now, &wh(), 1.0), 5);
        let b = (rank(&todo(), now, &wh(), 1.0), 2);
        assert_eq!(cmp_rank((&a.0, a.1), (&b.0, b.1)), std::cmp::Ordering::Greater);
        assert_eq!(top(&[a, b]), vec![1, 0]);
    }

    #[test]
    fn calibration_median_needs_five_samples() {
        assert_eq!(calibration(&[(120, 60); 4]), Calibration { factor: 1.0, samples: 4 });
        let s = [(90, 60), (120, 60), (60, 60), (150, 60), (84, 60)];
        // Ratios 1.5, 2, 1, 2.5, 1.4 → median 1.5.
        assert_eq!(calibration(&s), Calibration { factor: 1.5, samples: 5 });
        // Even count: mean of the middle two; zero values ignored.
        let s = [(60, 60), (120, 60), (60, 60), (120, 60), (90, 60), (90, 60), (0, 60)];
        assert_eq!(calibration(&s).factor, 1.5);
        assert_eq!(calibration(&s).samples, 6);
        // Clamped; only the newest 20 count.
        assert_eq!(calibration(&[(600, 60); 6]).factor, 3.0);
        assert_eq!(calibration(&[(10, 60); 6]).factor, 0.5);
        let mut many = vec![(60, 60); 20];
        many.extend([(600, 60); 10]);
        assert_eq!(calibration(&many), Calibration { factor: 1.0, samples: 20 });
    }
}
