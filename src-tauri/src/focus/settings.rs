//! Focus settings (spec §5.2): rows `focus.*` of the `settings` table. Lists
//! and the notification switches are stored as JSON, booleans as
//! `"true"` / `"false"`; a missing or unreadable row falls back to its default.

use super::score::WorkHours;
use crate::db::Db;
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const PREFIX: &str = "focus.";


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyKinds {
    pub morning: bool,
    pub start_by: bool,
    pub due_soon: bool,
    pub overdue: bool,
    pub remind: bool,
    pub snooze_end: bool,
}

impl Default for NotifyKinds {
    fn default() -> Self {
        NotifyKinds {
            morning: true,
            start_by: true,
            due_soon: true,
            overdue: true,
            remind: true,
            snooze_end: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusSettings {
    /// `HH:MM`.
    pub work_start: String,
    /// `HH:MM`, after `work_start`.
    pub work_end: String,
    /// ISO weekdays, 1 = Monday … 7 = Sunday.
    pub work_days: Vec<u8>,
    /// `HH:MM` of the morning summary.
    pub morning_time: String,
    pub notify_enabled: bool,
    pub notify_kinds: NotifyKinds,
    pub calendar_account_id: Option<String>,
    pub calendar_ids: Vec<String>,
    pub calibration_enabled: bool,
    /// Empty = the translate engine / model.
    pub ai_engine: String,
    pub ai_model: String,
}

impl Default for FocusSettings {
    fn default() -> Self {
        FocusSettings {
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            work_days: vec![1, 2, 3, 4, 5],
            morning_time: "09:00".into(),
            notify_enabled: true,
            notify_kinds: NotifyKinds::default(),
            calendar_account_id: None,
            calendar_ids: Vec::new(),
            calibration_enabled: true,
            ai_engine: String::new(),
            ai_model: String::new(),
        }
    }
}

fn hm(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    (s.len() == 5)
        .then(|| NaiveTime::parse_from_str(s, "%H:%M").ok())
        .flatten()
}

impl FocusSettings {
    /// Trim and check (start < end, at least one valid work day, `HH:MM`).
    pub fn validated(mut self) -> Result<Self, String> {
        let start = hm(&self.work_start).ok_or("work_start must be HH:MM")?;
        let end = hm(&self.work_end).ok_or("work_end must be HH:MM")?;
        if start >= end {
            return Err("work_end must be after work_start".into());
        }
        hm(&self.morning_time).ok_or("morning_time must be HH:MM")?;
        self.work_start = self.work_start.trim().into();
        self.work_end = self.work_end.trim().into();
        self.morning_time = self.morning_time.trim().into();
        self.work_days.sort_unstable();
        self.work_days.dedup();
        if self.work_days.is_empty() || self.work_days.iter().any(|d| !(1..=7).contains(d)) {
            return Err("work_days must list weekdays 1 (Monday) to 7 (Sunday)".into());
        }
        self.calendar_account_id = self
            .calendar_account_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.calendar_ids.retain(|s| !s.trim().is_empty());
        self.ai_engine = self.ai_engine.trim().into();
        self.ai_model = self.ai_model.trim().into();
        Ok(self)
    }

    pub fn work_hours(&self) -> WorkHours {
        let d = WorkHours::default();
        let mut days = [false; 7];
        for n in &self.work_days {
            if (1..=7).contains(n) {
                days[*n as usize - 1] = true;
            }
        }
        WorkHours {
            start: hm(&self.work_start).unwrap_or(d.start),
            end: hm(&self.work_end).unwrap_or(d.end),
            days,
        }
    }

    fn from_rows(rows: &HashMap<String, String>) -> Self {
        let mut s = FocusSettings::default();
        let get = |k: &str| rows.get(&format!("{PREFIX}{k}")).map(String::as_str);
        fn json<T: serde::de::DeserializeOwned>(v: Option<&str>) -> Option<T> {
            serde_json::from_str(v?).ok()
        }
        let bool_of = |k: &str, d: bool| get(k).map_or(d, |v| v == "true");
        if let Some(v) = get("work_start").filter(|v| hm(v).is_some()) {
            s.work_start = v.into();
        }
        if let Some(v) = get("work_end").filter(|v| hm(v).is_some()) {
            s.work_end = v.into();
        }
        if hm(&s.work_start) >= hm(&s.work_end) {
            s.work_start = "09:00".into();
            s.work_end = "18:00".into();
        }
        if let Some(v) = json::<Vec<u8>>(get("work_days")).filter(|v: &Vec<u8>| {
            !v.is_empty() && v.iter().all(|d| (1..=7).contains(d))
        }) {
            s.work_days = v;
        }
        if let Some(v) = get("morning_time").filter(|v| hm(v).is_some()) {
            s.morning_time = v.into();
        }
        s.notify_enabled = bool_of("notify_enabled", true);
        if let Some(v) = json(get("notify_kinds")) {
            s.notify_kinds = v;
        }
        s.calendar_account_id = get("calendar_account_id")
            .filter(|v| !v.is_empty())
            .map(Into::into);
        if let Some(v) = json(get("calendar_ids")) {
            s.calendar_ids = v;
        }
        s.calibration_enabled = bool_of("calibration_enabled", true);
        s.ai_engine = get("ai_engine").unwrap_or("").into();
        s.ai_model = get("ai_model").unwrap_or("").into();
        s
    }

    fn to_rows(&self) -> Vec<(String, String)> {
        let b = |v: bool| if v { "true" } else { "false" }.to_string();
        let j = |v: serde_json::Value| v.to_string();
        [
            ("work_start", self.work_start.clone()),
            ("work_end", self.work_end.clone()),
            ("work_days", j(serde_json::json!(self.work_days))),
            ("morning_time", self.morning_time.clone()),
            ("notify_enabled", b(self.notify_enabled)),
            ("notify_kinds", j(serde_json::json!(self.notify_kinds))),
            (
                "calendar_account_id",
                self.calendar_account_id.clone().unwrap_or_default(),
            ),
            ("calendar_ids", j(serde_json::json!(self.calendar_ids))),
            ("calibration_enabled", b(self.calibration_enabled)),
            ("ai_engine", self.ai_engine.clone()),
            ("ai_model", self.ai_model.clone()),
        ]
        .into_iter()
        .map(|(k, v)| (format!("{PREFIX}{k}"), v))
        .collect()
    }
}

pub async fn load(db: &Db) -> Result<FocusSettings, String> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings WHERE key LIKE 'focus.%'")
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;
    Ok(FocusSettings::from_rows(&rows.into_iter().collect()))
}

pub async fn save(db: &Db, settings: FocusSettings) -> Result<FocusSettings, String> {
    let settings = settings.validated()?;
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;
    for (k, v) in settings.to_rows() {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(k)
        .bind(v)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_work_hours() {
        let s = FocusSettings::default();
        assert_eq!(s.work_hours(), WorkHours::default());
        assert!(s.notify_kinds.overdue && s.calibration_enabled && s.notify_enabled);
        assert_eq!(FocusSettings::from_rows(&HashMap::new()), s);
    }

    #[test]
    fn rows_round_trip_and_bad_rows_fall_back() {
        let mut s = FocusSettings {
            work_start: "08:30".into(),
            work_days: vec![1, 2, 3, 4, 5, 6],
            calendar_account_id: Some("acc".into()),
            calendar_ids: vec!["primary".into()],
            calibration_enabled: false,
            ..Default::default()
        };
        s.notify_kinds.morning = false;
        let rows: HashMap<String, String> = s.to_rows().into_iter().collect();
        assert!(rows.keys().all(|k| k.starts_with(PREFIX)));
        // Backups / sync carry every key but the per-machine calendar choice.
        use crate::commands::settings::PORTABLE_SETTING_KEYS;
        for k in rows.keys() {
            let portable = PORTABLE_SETTING_KEYS.contains(&k.as_str());
            assert_eq!(portable, !k.starts_with("focus.calendar_"), "{k}");
        }
        assert_eq!(FocusSettings::from_rows(&rows), s);
        let bad: HashMap<String, String> = [
            ("focus.work_start", "19:00"),
            ("focus.work_days", "[]"),
            ("focus.notify_kinds", "nope"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(FocusSettings::from_rows(&bad), FocusSettings::default());
    }

    #[test]
    fn validation() {
        let ok = |s: FocusSettings| s.validated();
        assert!(ok(FocusSettings::default()).is_ok());
        let bad_order = FocusSettings { work_start: "18:00".into(), work_end: "09:00".into(), ..Default::default() };
        assert!(ok(bad_order).is_err());
        assert!(ok(FocusSettings { work_days: vec![], ..Default::default() }).is_err());
        assert!(ok(FocusSettings { work_days: vec![0], ..Default::default() }).is_err());
        assert!(ok(FocusSettings { morning_time: "9:00".into(), ..Default::default() }).is_err());
        let v = ok(FocusSettings {
            work_days: vec![3, 1, 1],
            calendar_account_id: Some("  ".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(v.work_days, vec![1, 3]);
        assert_eq!(v.calendar_account_id, None);
    }
}
