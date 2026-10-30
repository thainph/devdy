//! Hybrid logical clock: 48-bit wall millis + 16-bit counter + device id.
//! Total order: (millis, counter, device). String form (design §9.1):
//! `"<12 hex millis><4 hex counter>-<device_id>"`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::SqliteConnection;

const MAX_MILLIS: u64 = (1 << 48) - 1;
const STATE_KEY: &str = "hlc_last";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    pub millis: u64,
    pub counter: u16,
    pub device: String,
}

impl Hlc {
    pub fn new(millis: u64, counter: u16, device: impl Into<String>) -> Self {
        Hlc { millis: millis.min(MAX_MILLIS), counter, device: device.into() }
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.millis, self.counter, &self.device).cmp(&(other.millis, other.counter, &other.device))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:012x}{:04x}-{}", self.millis, self.counter, self.device)
    }
}

impl FromStr for Hlc {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("invalid hlc: {s:?}");
        if s.len() < 18 || !s.is_char_boundary(16) || s.as_bytes()[16] != b'-' {
            return Err(bad());
        }
        let millis = u64::from_str_radix(&s[..12], 16).map_err(|_| bad())?;
        let counter = u16::from_str_radix(&s[12..16], 16).map_err(|_| bad())?;
        let device = &s[17..];
        if device.is_empty() {
            return Err(bad());
        }
        Ok(Hlc { millis, counter, device: device.to_string() })
    }
}

impl Serialize for Hlc {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

pub fn wall_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// Issues HLCs for this device. Not thread-safe by itself; the store keeps it
/// behind a mutex and persists it in `sync_state`.
#[derive(Debug, Clone)]
pub struct HlcClock {
    device: String,
    millis: u64,
    counter: u16,
}

impl HlcClock {
    pub fn new(device: impl Into<String>) -> Self {
        HlcClock { device: device.into(), millis: 0, counter: 0 }
    }

    #[allow(dead_code)]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Last issued / observed value.
    pub fn last(&self) -> Hlc {
        Hlc::new(self.millis, self.counter, self.device.clone())
    }

    /// A new HLC for a local event happening now.
    #[allow(dead_code)]
    pub fn now(&mut self) -> Hlc {
        self.tick_at(wall_ms())
    }

    /// A new HLC for a local event that happened at `physical_ms`: greater than
    /// everything issued or observed so far.
    pub fn tick_at(&mut self, physical_ms: u64) -> Hlc {
        let physical_ms = physical_ms.min(MAX_MILLIS);
        if physical_ms > self.millis {
            self.millis = physical_ms;
            self.counter = 0;
        } else if self.counter == u16::MAX {
            self.millis = (self.millis + 1).min(MAX_MILLIS);
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        self.last()
    }

    /// Merge a remote HLC so every later local tick orders after it.
    pub fn observe(&mut self, remote: &Hlc) {
        if (remote.millis, remote.counter) > (self.millis, self.counter) {
            self.millis = remote.millis.min(MAX_MILLIS);
            self.counter = remote.counter;
        }
    }

    pub async fn load(conn: &mut SqliteConnection, device: &str) -> Result<Self, String> {
        let mut clock = HlcClock::new(device);
        let saved: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
            .bind(STATE_KEY)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(h) = saved.and_then(|s| s.parse::<Hlc>().ok()) {
            clock.observe(&h);
        }
        Ok(clock)
    }

    pub async fn save(&self, conn: &mut SqliteConnection) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO sync_state (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(STATE_KEY)
        .bind(self.last().to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_form_round_trips_and_sorts() {
        let h = Hlc::new(0x18f_0000_0001, 7, "dev-a");
        assert_eq!(h.to_string(), "018f000000010007-dev-a");
        assert_eq!(h.to_string().parse::<Hlc>().unwrap(), h);
        assert!("xyz".parse::<Hlc>().is_err());
        assert!("018f000000010007-".parse::<Hlc>().is_err());
    }

    #[test]
    fn total_ordering() {
        let a = Hlc::new(10, 0, "a");
        let b = Hlc::new(10, 0, "b");
        let c = Hlc::new(10, 1, "a");
        let d = Hlc::new(11, 0, "a");
        assert!(a < b && b < c && c < d);
        // The string form sorts the same way as the struct.
        let mut v = vec![d.to_string(), b.to_string(), c.to_string(), a.to_string()];
        v.sort();
        assert_eq!(v, vec![a.to_string(), b.to_string(), c.to_string(), d.to_string()]);
    }

    #[test]
    fn ticks_are_monotonic_even_when_wall_time_goes_back() {
        let mut c = HlcClock::new("me");
        let h1 = c.tick_at(1000);
        let h2 = c.tick_at(900);
        let h3 = c.tick_at(1000);
        let h4 = c.tick_at(1001);
        assert!(h1 < h2 && h2 < h3 && h3 < h4);
        assert_eq!((h2.millis, h2.counter), (1000, 1));
        assert_eq!((h4.millis, h4.counter), (1001, 0));
    }

    #[test]
    fn observe_orders_later_ticks_after_remote() {
        let mut c = HlcClock::new("me");
        c.tick_at(1000);
        let remote = Hlc::new(5000, 3, "other");
        c.observe(&remote);
        let next = c.tick_at(1200);
        assert!(next > remote);
        assert_eq!((next.millis, next.counter), (5000, 4));
    }

    #[test]
    fn counter_overflow_bumps_millis() {
        let mut c = HlcClock::new("me");
        c.observe(&Hlc::new(100, u16::MAX, "x"));
        let h = c.tick_at(50);
        assert_eq!((h.millis, h.counter), (101, 0));
    }
}
