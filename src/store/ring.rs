//! Fixed-size ring of time buckets, one `f32` per bucket (NaN means no data).

/// Values for the most recent `len` buckets of `step` seconds each.
#[derive(Debug, Clone)]
pub struct Ring {
    step: u64,
    vals: Box<[f32]>,
    newest: Option<u64>,
}

impl Ring {
    /// Creates an empty ring covering `len` buckets of `step` seconds.
    pub fn new(step: u64, len: usize) -> Self {
        Ring { step, vals: vec![f32::NAN; len.max(1)].into_boxed_slice(), newest: None }
    }

    /// Bucket length in seconds.
    pub fn step(&self) -> u64 {
        self.step
    }

    /// Number of buckets kept.
    pub fn len(&self) -> usize {
        self.vals.len()
    }

    /// Index of the newest written bucket, if any.
    pub fn newest(&self) -> Option<u64> {
        self.newest
    }

    /// Stores `v` in `bucket`; buckets older than the newest are ignored so history never rewinds.
    pub fn put(&mut self, bucket: u64, v: f32) {
        let len = self.vals.len() as u64;
        match self.newest {
            Some(n) if bucket < n => return,
            Some(n) if bucket > n => {
                // Blank skipped buckets so stale values from a previous lap never reappear.
                let gap = (bucket - n - 1).min(len);
                for b in (bucket - gap)..bucket {
                    self.vals[(b % len) as usize] = f32::NAN;
                }
            }
            _ => {}
        }
        self.vals[(bucket % len) as usize] = v;
        self.newest = Some(bucket);
    }

    /// Value in `bucket`, or NaN when unknown or out of the kept window.
    pub fn get(&self, bucket: u64) -> f32 {
        let len = self.vals.len() as u64;
        match self.newest {
            Some(n) if bucket <= n && n - bucket < len => self.vals[(bucket % len) as usize],
            _ => f32::NAN,
        }
    }

    /// Values from oldest to newest, for saving to disk.
    pub fn ordered(&self) -> Vec<f32> {
        let Some(n) = self.newest else { return vec![f32::NAN; self.vals.len()] };
        let len = self.vals.len() as u64;
        let first = (n + 1).saturating_sub(len);
        (first..first + len).map(|b| self.get(b)).collect()
    }

    /// Rebuilds a ring from `ordered()` output whose last value sits in bucket `newest`.
    pub fn restore(step: u64, len: usize, newest: u64, ordered: &[f32]) -> Self {
        let mut r = Ring::new(step, len);
        let start = (newest + 1).saturating_sub(ordered.len() as u64);
        for (i, v) in ordered.iter().enumerate() {
            r.put(start + i as u64, *v);
        }
        r.newest = Some(newest);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reads_back() {
        let mut r = Ring::new(5, 4);
        r.put(10, 1.0);
        r.put(11, 2.0);
        assert_eq!(r.get(10), 1.0);
        assert_eq!(r.get(11), 2.0);
        assert!(r.get(12).is_nan());
        assert!(r.get(9).is_nan());
    }

    #[test]
    fn gaps_are_blank_not_stale() {
        let mut r = Ring::new(5, 4);
        for b in 0..4 {
            r.put(b, b as f32);
        }
        r.put(6, 6.0); // skips 4 and 5, which reuse slots of 0 and 1
        assert!(r.get(4).is_nan());
        assert!(r.get(5).is_nan());
        assert_eq!(r.get(3), 3.0);
        assert!(r.get(2).is_nan(), "fell out of the window");
    }

    #[test]
    fn clock_going_backwards_never_rewrites() {
        let mut r = Ring::new(5, 4);
        r.put(10, 1.0);
        r.put(8, 99.0);
        assert!(r.get(8).is_nan());
        assert_eq!(r.newest(), Some(10));
    }

    #[test]
    fn huge_jump_clears_everything() {
        let mut r = Ring::new(5, 4);
        r.put(1, 1.0);
        r.put(1_000_000, 2.0);
        assert_eq!(r.ordered()[..3].iter().filter(|v| v.is_nan()).count(), 3);
    }

    #[test]
    fn ordered_roundtrip() {
        let mut r = Ring::new(60, 5);
        for b in 100..107 {
            r.put(b, b as f32);
        }
        let back = Ring::restore(60, 5, 106, &r.ordered());
        for b in 100..107 {
            assert_eq!(back.get(b).to_bits(), r.get(b).to_bits());
        }
    }
}
