//! Per-edge byte-rate smoothing via exponential moving average.
//!
//! Each flow record contributes its bytes/duration as a sample. The
//! EMA blends the new sample with the existing rate using a coefficient
//! derived from a configurable half-life. With a 5s half-life, half
//! the influence of a sample decays after 5s — smooth enough that the
//! UI doesn't flicker, responsive enough that bursts are visible.

use std::time::{Duration, SystemTime};

/// Smooths a stream of samples (bytes, duration) into a current rate.
#[derive(Debug, Clone)]
pub struct RateMeter {
    half_life: Duration,
    bytes_per_sec: f64,
    last_update: SystemTime,
    has_sample: bool,
}

impl RateMeter {
    pub fn new(half_life: Duration, now: SystemTime) -> Self {
        Self {
            half_life,
            bytes_per_sec: 0.0,
            last_update: now,
            has_sample: false,
        }
    }

    pub fn current(&self) -> f64 {
        self.bytes_per_sec
    }

    /// Record a flow that delivered `bytes` over `flow_duration`,
    /// observed at `now`. The sample is blended with the existing
    /// rate weighted by the time since the last update.
    pub fn observe(&mut self, bytes: u64, flow_duration: Duration, now: SystemTime) {
        let sample_rate = if flow_duration.is_zero() {
            // Single-packet or sub-millisecond flow: attribute all
            // bytes to a 1ms window so we don't divide by zero.
            (bytes as f64) * 1000.0
        } else {
            (bytes as f64) / flow_duration.as_secs_f64()
        };

        if !self.has_sample {
            // First sample becomes the rate directly — there's no
            // prior data to blend with.
            self.bytes_per_sec = sample_rate;
            self.has_sample = true;
        } else {
            let elapsed = now
                .duration_since(self.last_update)
                .unwrap_or(Duration::ZERO);
            let alpha = sample_weight(elapsed, self.half_life);
            self.bytes_per_sec = alpha * sample_rate + (1.0 - alpha) * self.bytes_per_sec;
        }
        self.last_update = now;
    }

    /// Decay the rate towards zero based on time elapsed since the
    /// last sample. Called on snapshot generation so an edge that
    /// stopped sending shows zero rate within a few half-lives.
    pub fn decay_to(&mut self, now: SystemTime) {
        let elapsed = now
            .duration_since(self.last_update)
            .unwrap_or(Duration::ZERO);
        let alpha = sample_weight(elapsed, self.half_life);
        self.bytes_per_sec *= 1.0 - alpha;
        self.last_update = now;
    }
}

/// Weight for a new sample given how long since the last one. The
/// formula `1 - 0.5^(elapsed/half_life)` gives the fraction of the
/// stream's "memory" that should be replaced by the new sample.
fn sample_weight(elapsed: Duration, half_life: Duration) -> f64 {
    if half_life.is_zero() {
        return 1.0;
    }
    let ratio = elapsed.as_secs_f64() / half_life.as_secs_f64();
    1.0 - 0.5_f64.powf(ratio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn fresh_meter_has_zero_rate() {
        let m = RateMeter::new(Duration::from_secs(5), t(0));
        assert_eq!(m.current(), 0.0);
    }

    #[test]
    fn first_sample_becomes_the_rate() {
        let mut m = RateMeter::new(Duration::from_secs(5), t(0));
        // 1000 bytes over 1s = 1000 bytes/sec. First sample should
        // become the rate exactly (no prior data to blend with).
        m.observe(1000, Duration::from_secs(1), t(0));
        assert_eq!(m.current(), 1000.0);
    }

    #[test]
    fn second_sample_blends_with_first() {
        let mut m = RateMeter::new(Duration::from_secs(5), t(0));
        m.observe(1000, Duration::from_secs(1), t(0)); // rate=1000
                                                       // 5s later (one half-life): alpha=0.5
                                                       // sample_rate = 500/1 = 500
                                                       // new rate = 0.5*500 + 0.5*1000 = 750
        m.observe(500, Duration::from_secs(1), t(5));
        assert!((m.current() - 750.0).abs() < 1.0);
    }

    #[test]
    fn decay_reduces_rate_over_time() {
        let mut m = RateMeter::new(Duration::from_secs(5), t(0));
        m.observe(1000, Duration::from_secs(1), t(0));
        let initial = m.current();
        // After one half-life, rate should be approximately halved.
        m.decay_to(t(5));
        assert!(m.current() < initial / 2.0 + 1.0);
        assert!(m.current() > initial / 2.0 - 1.0);
    }

    #[test]
    fn zero_duration_flow_doesnt_panic() {
        let mut m = RateMeter::new(Duration::from_secs(5), t(0));
        m.observe(500, Duration::ZERO, t(0));
        assert!(m.current() > 0.0);
    }

    #[test]
    fn sample_weight_at_zero_elapsed_is_zero() {
        // No time has passed since last sample => new sample doesn't
        // overwrite anything.
        assert_eq!(sample_weight(Duration::ZERO, Duration::from_secs(5)), 0.0);
    }

    #[test]
    fn sample_weight_at_one_half_life_is_half() {
        let w = sample_weight(Duration::from_secs(5), Duration::from_secs(5));
        assert!((w - 0.5).abs() < 1e-9);
    }
}
