//! Today's workload against the Google Calendar (spec §3.5, BR-08), pure.
//!
//! `need` = Σ effective estimate of the todos that are doing / overdue / due
//! today. `free` = working minutes left today minus busy events (all-day,
//! `transparency = transparent` and declined events ignored; overlaps merged).

use super::score::{Bucket, WorkHours};
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use serde::Serialize;

/// One Calendar event as fetched, times already in local wall-clock.
#[derive(Debug, Clone, PartialEq)]
pub struct CalEvent {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub title: String,
    pub all_day: bool,
    pub transparent: bool,
    pub declined: bool,
}

/// A busy block of today, clipped to the day.
#[derive(Debug, Clone, PartialEq)]
pub struct Busy {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BusyOut {
    /// RFC3339, local offset.
    pub start: String,
    pub end: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capacity {
    pub need_min: i64,
    pub free_min: i64,
    pub busy: Vec<BusyOut>,
}

/// The events that make `day` busy, clipped to the day, by start.
pub fn busy_on(events: &[CalEvent], day: NaiveDate) -> Vec<Busy> {
    let (ds, de) = (
        day.and_hms_opt(0, 0, 0).unwrap(),
        day.succ_opt().unwrap_or(day).and_hms_opt(0, 0, 0).unwrap(),
    );
    let mut out: Vec<Busy> = events
        .iter()
        .filter(|e| !e.all_day && !e.transparent && !e.declined)
        .filter_map(|e| {
            let (s, en) = (e.start.max(ds), e.end.min(de));
            (en > s).then(|| Busy {
                start: s,
                end: en,
                title: e.title.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    out
}

/// Union of the intervals (sorted, non-overlapping).
pub fn union(busy: &[Busy]) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    let mut v: Vec<(NaiveDateTime, NaiveDateTime)> = busy.iter().map(|b| (b.start, b.end)).collect();
    v.sort();
    let mut out: Vec<(NaiveDateTime, NaiveDateTime)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Working minutes left today (`now` → end of work) not covered by `busy`.
pub fn free_minutes(now: NaiveDateTime, wh: &WorkHours, busy: &[Busy]) -> i64 {
    let day = now.date();
    if !wh.days[day.weekday().num_days_from_monday() as usize] || wh.start >= wh.end {
        return 0;
    }
    let ws = day.and_time(wh.start).max(now);
    let we = day.and_time(wh.end);
    if we <= ws {
        return 0;
    }
    let taken: i64 = union(busy)
        .into_iter()
        .map(|(s, e)| {
            let (s, e) = (s.max(ws), e.min(we));
            if e > s { (e - s).num_minutes() } else { 0 }
        })
        .sum();
    (we - ws).num_minutes() - taken
}

/// Σ effective estimate of doing / overdue / today todos.
pub fn need_minutes<'a>(items: impl IntoIterator<Item = (Bucket, i64)> + 'a) -> i64 {
    items
        .into_iter()
        .filter(|(b, _)| matches!(b, Bucket::Doing | Bucket::Overdue | Bucket::Today))
        .map(|(_, m)| m)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn ev(s: &str, e: &str, title: &str) -> CalEvent {
        CalEvent {
            start: at(s),
            end: at(e),
            title: title.into(),
            all_day: false,
            transparent: false,
            declined: false,
        }
    }

    #[test]
    fn busy_filters_and_clips() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let events = vec![
            ev("2026-10-28 13:00", "2026-10-28 14:00", "Standup"),
            CalEvent { all_day: true, ..ev("2026-10-28 00:00", "2026-10-29 00:00", "Holiday") },
            CalEvent { transparent: true, ..ev("2026-10-28 10:00", "2026-10-28 11:00", "FYI") },
            CalEvent { declined: true, ..ev("2026-10-28 15:00", "2026-10-28 16:00", "Nope") },
            ev("2026-10-27 23:00", "2026-10-28 09:30", "Overnight"),
            ev("2026-10-29 10:00", "2026-10-29 11:00", "Tomorrow"),
        ];
        let b = busy_on(&events, day);
        let titles: Vec<&str> = b.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, vec!["Overnight", "Standup"]);
        assert_eq!(b[0].start, at("2026-10-28 00:00"));
    }

    #[test]
    fn union_merges_overlaps_and_touching() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let b = busy_on(
            &[
                ev("2026-10-28 10:00", "2026-10-28 11:00", "a"),
                ev("2026-10-28 10:30", "2026-10-28 12:00", "b"),
                ev("2026-10-28 12:00", "2026-10-28 12:30", "c"),
                ev("2026-10-28 14:00", "2026-10-28 15:00", "d"),
            ],
            day,
        );
        assert_eq!(
            union(&b),
            vec![
                (at("2026-10-28 10:00"), at("2026-10-28 12:30")),
                (at("2026-10-28 14:00"), at("2026-10-28 15:00")),
            ]
        );
    }

    #[test]
    fn free_minutes_counts_remaining_work_time() {
        let wh = WorkHours::default();
        let day = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let b = busy_on(
            &[
                ev("2026-10-28 08:00", "2026-10-28 09:30", "early"),
                ev("2026-10-28 13:00", "2026-10-28 14:00", "lunch"),
                ev("2026-10-28 13:30", "2026-10-28 14:30", "overlap"),
                ev("2026-10-28 17:30", "2026-10-28 19:00", "late"),
            ],
            day,
        );
        // 09:00–18:00 = 540 − 30 (early) − 90 (13:00–14:30) − 30 (17:30–18:00).
        assert_eq!(free_minutes(at("2026-10-28 07:00"), &wh, &b), 390);
        // From 13:15: 285 left − 75 − 30.
        assert_eq!(free_minutes(at("2026-10-28 13:15"), &wh, &b), 180);
        assert_eq!(free_minutes(at("2026-10-28 18:30"), &wh, &b), 0);
        // Saturday.
        assert_eq!(free_minutes(at("2026-10-31 10:00"), &wh, &[]), 0);
    }

    #[test]
    fn need_counts_doing_overdue_today() {
        let n = need_minutes([
            (Bucket::Doing, 60),
            (Bucket::Overdue, 30),
            (Bucket::Today, 45),
            (Bucket::Tomorrow, 600),
            (Bucket::Snoozed, 600),
            (Bucket::Done, 600),
        ]);
        assert_eq!(n, 135);
    }
}
