//! Metric history: every series keeps three downsampled tiers in fixed-size rings.

mod persist;
mod ring;

use std::collections::HashMap;

use serde::Serialize;

pub use persist::{encode, load, write};
use ring::Ring;

/// (bucket seconds, bucket count) for each tier: 5s×1h, 1m×48h, 15m×30d.
const TIERS: [(u64, usize); 3] = [(5, 720), (60, 2880), (900, 2880)];
/// Hard cap (about 16 MB of rings) so container churn cannot grow memory without bound.
pub const MAX_SERIES: usize = 600;
/// Series not updated for this long are dropped.
const IDLE_DROP_SECS: u64 = 30 * 24 * 3600;
/// Apps that disappeared (for example short-lived containers) are dropped sooner.
const APP_IDLE_DROP_SECS: u64 = 7 * 24 * 3600;

/// A time window the UI can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    H1,
    H24,
    D7,
    D30,
}

impl Range {
    /// Parses "1h", "24h", "7d" or "30d".
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1h" => Some(Range::H1),
            "24h" => Some(Range::H24),
            "7d" => Some(Range::D7),
            "30d" => Some(Range::D30),
            _ => None,
        }
    }

    /// Window length in seconds.
    pub fn secs(self) -> u64 {
        match self {
            Range::H1 => 3600,
            Range::H24 => 86_400,
            Range::D7 => 7 * 86_400,
            Range::D30 => 30 * 86_400,
        }
    }

    /// Tier that answers this range with a sensible number of points.
    fn tier(self) -> usize {
        match self {
            Range::H1 => 0,
            Range::H24 => 1,
            Range::D7 | Range::D30 => 2,
        }
    }
}

/// One averaging tier: a ring plus the bucket still being filled.
#[derive(Debug, Clone)]
struct Tier {
    ring: Ring,
    acc_bucket: u64,
    acc_sum: f64,
    acc_n: u32,
}

impl Tier {
    fn new(step: u64, len: usize) -> Self {
        Tier { ring: Ring::new(step, len), acc_bucket: 0, acc_sum: 0.0, acc_n: 0 }
    }

    /// Adds a sample; a finished bucket is flushed as its average.
    fn push(&mut self, ts: u64, v: f32) {
        let b = ts / self.ring.step();
        if self.acc_n > 0 && b > self.acc_bucket {
            self.ring.put(self.acc_bucket, (self.acc_sum / self.acc_n as f64) as f32);
            self.acc_n = 0;
            self.acc_sum = 0.0;
        }
        if self.acc_n == 0 {
            // Never start a bucket behind what is already stored (clock moved back).
            self.acc_bucket = self.ring.newest().map_or(b, |n| b.max(n + 1));
        }
        self.acc_sum += v as f64;
        self.acc_n += 1;
    }

    /// Value of a bucket, including the partial one being filled.
    fn get(&self, b: u64) -> f32 {
        if self.acc_n > 0 && b == self.acc_bucket {
            (self.acc_sum / self.acc_n as f64) as f32
        } else {
            self.ring.get(b)
        }
    }
}

/// All tiers of one metric.
#[derive(Debug, Clone)]
pub struct Series {
    tiers: [Tier; 3],
    last_ts: u64,
}

impl Series {
    fn new() -> Self {
        Series { tiers: TIERS.map(|(s, l)| Tier::new(s, l)), last_ts: 0 }
    }
}

/// Points for a chart: `values[i]` belongs to time `start + i * step`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Points {
    pub start: u64,
    pub step: u64,
    pub values: Vec<Option<f32>>,
}

impl Points {
    /// Known values only.
    pub fn known(&self) -> impl Iterator<Item = f32> + '_ {
        self.values.iter().flatten().copied()
    }
}

/// Every series by key, e.g. `host.cpu` or `app:docker:web.mem`.
#[derive(Debug, Default)]
pub struct History {
    series: HashMap<String, Series>,
}

impl History {
    /// Records `v` for `key` at unix time `ts`; non-finite values are skipped.
    pub fn record(&mut self, key: &str, ts: u64, v: f64) {
        if !v.is_finite() {
            return;
        }
        if !self.series.contains_key(key) {
            if self.series.len() >= MAX_SERIES {
                // Full: make room by evicting the series that has been quiet the longest.
                let stalest = self.series.iter().min_by_key(|(_, s)| s.last_ts).map(|(k, _)| k.clone());
                if let Some(k) = stalest {
                    self.series.remove(&k);
                }
            }
            self.series.insert(key.to_string(), Series::new());
        }
        if let Some(s) = self.series.get_mut(key) {
            for t in &mut s.tiers {
                t.push(ts, v as f32);
            }
            s.last_ts = s.last_ts.max(ts);
        }
    }

    /// Points for `key` covering `range`, ending at `now`.
    pub fn query(&self, key: &str, range: Range, now: u64) -> Option<Points> {
        let s = self.series.get(key)?;
        let tier = &s.tiers[range.tier()];
        let step = tier.ring.step();
        let last = now / step;
        let count = (range.secs() / step).min(tier.ring.len() as u64);
        let first = (last + 1).saturating_sub(count);
        let values = (first..=last).map(|b| Some(tier.get(b)).filter(|v| v.is_finite())).collect();
        Some(Points { start: first * step, step, values })
    }

    /// Whether any series exists for `key`.
    pub fn has(&self, key: &str) -> bool {
        self.series.contains_key(key)
    }

    /// Drops series idle for 30 days, or 7 days for apps that are gone.
    pub fn prune(&mut self, now: u64) {
        self.series.retain(|k, s| {
            let limit = if k.starts_with("app:") { APP_IDLE_DROP_SECS } else { IDLE_DROP_SECS };
            now.saturating_sub(s.last_ts) < limit
        });
    }

    /// Number of stored series.
    pub fn len(&self) -> usize {
        self.series.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_within_a_bucket() {
        let mut h = History::default();
        h.record("k", 60, 10.0);
        h.record("k", 61, 20.0);
        h.record("k", 125, 5.0);
        let p = h.query("k", Range::H24, 125).unwrap();
        assert_eq!(p.step, 60);
        let n = p.values.len();
        assert_eq!(p.values[n - 1], Some(5.0), "partial bucket is visible");
        assert_eq!(p.values[n - 2], Some(15.0), "finished bucket holds the average");
        assert_eq!(p.values[n - 3], None);
    }

    #[test]
    fn query_shape_matches_range() {
        let mut h = History::default();
        let now = 1_700_000_000;
        h.record("k", now, 1.0);
        let p = h.query("k", Range::H1, now).unwrap();
        assert_eq!(p.values.len(), 720);
        assert_eq!(p.step, 5);
        assert_eq!(*p.values.last().unwrap(), Some(1.0));
        let p = h.query("k", Range::D7, now).unwrap();
        assert_eq!(p.values.len(), 672);
    }

    #[test]
    fn skips_nan_and_caps_series() {
        let mut h = History::default();
        h.record("nan", 1, f64::NAN);
        assert!(!h.has("nan"));
        for i in 0..MAX_SERIES + 10 {
            h.record(&format!("s{i}"), 1 + i as u64, 1.0);
        }
        assert_eq!(h.len(), MAX_SERIES);
        assert!(!h.has("s0"), "the stalest series made room");
        assert!(h.has(&format!("s{}", MAX_SERIES + 9)), "new series are still recorded");
    }

    #[test]
    fn backwards_clock_keeps_old_buckets() {
        let mut h = History::default();
        h.record("k", 600, 1.0);
        h.record("k", 700, 2.0); // flushes bucket 10 (600..660) in the 1m tier
        h.record("k", 100, 50.0); // clock jumped back; must not touch bucket 10
        let p = h.query("k", Range::H24, 700).unwrap();
        let at = |ts: u64| p.values[((ts - p.start) / p.step) as usize];
        assert_eq!(at(600), Some(1.0));
    }

    #[test]
    fn prune_drops_idle() {
        let mut h = History::default();
        h.record("old", 1, 1.0);
        h.record("new", IDLE_DROP_SECS + 5, 1.0);
        h.prune(IDLE_DROP_SECS + 10);
        assert!(!h.has("old"));
        assert!(h.has("new"));
    }
}
