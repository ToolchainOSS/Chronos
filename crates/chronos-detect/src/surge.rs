//! Temporal surge monitor (blueprint Task 3.3).
//!
//! For each prefix a sliding window (a ring buffer of update timestamps) tracks
//! how many updates arrived within the last `window_secs` seconds. The velocity
//! (the window count) is compared against an adaptive high pass threshold derived
//! from the Median Absolute Deviation (MAD) of recent per prefix window counts.
//! A prefix whose velocity crosses that threshold is flagged as route churn.
//!
//! This monitor is single threaded by design; it runs on the single consumer task
//! that drains the ingestion channel, so no internal locking is required.

use crate::anomaly::{Anomaly, Severity};
use chronos_types::IpPrefix;
use std::collections::{HashMap, VecDeque};

/// Configuration for the surge monitor.
#[derive(Debug, Clone)]
pub struct SurgeConfig {
    /// Sliding window length in seconds (the blueprint specifies 10 seconds).
    pub window_secs: f64,
    /// The MAD multiplier `k`; the threshold is `median + k * (1.4826 * MAD)`.
    pub k: f64,
    /// The minimum number of baseline samples before the adaptive threshold is
    /// used; below this a fixed floor applies.
    pub min_samples: usize,
    /// The maximum number of recent window counts retained for the baseline.
    pub baseline_capacity: usize,
    /// A hard floor: a prefix must have at least this many updates in the window
    /// before it can be flagged (suppresses noise on low volume prefixes).
    pub min_updates: u32,
}

impl Default for SurgeConfig {
    fn default() -> Self {
        Self {
            window_secs: 10.0,
            k: 6.0,
            min_samples: 32,
            baseline_capacity: 1024,
            min_updates: 8,
        }
    }
}

/// Tracks per prefix update velocity and flags route churn.
pub struct SurgeMonitor {
    config: SurgeConfig,
    windows: HashMap<IpPrefix, VecDeque<f64>>,
    baseline: VecDeque<f64>,
}

impl SurgeMonitor {
    /// Create a monitor with the given configuration.
    pub fn new(config: SurgeConfig) -> Self {
        Self {
            config,
            windows: HashMap::new(),
            baseline: VecDeque::new(),
        }
    }

    /// Record an update for `prefix` observed at `now_secs` (a Unix timestamp).
    ///
    /// Returns a `RouteChurn` anomaly when the prefix velocity crosses the
    /// adaptive threshold.
    pub fn record(&mut self, prefix: IpPrefix, now_secs: f64) -> Option<Anomaly> {
        let window = self.config.window_secs;
        let cutoff = now_secs - window;

        let ring = self.windows.entry(prefix).or_default();
        ring.push_back(now_secs);
        // Prune timestamps that fell out of the trailing window.
        while ring.front().is_some_and(|&t| t < cutoff) {
            ring.pop_front();
        }
        let count = ring.len() as u32;

        // Feed the baseline population used to derive the adaptive threshold.
        self.push_baseline(count as f64);

        if count < self.config.min_updates {
            return None;
        }

        let threshold = self.threshold();
        if (count as f64) > threshold {
            let severity = classify(count as f64, threshold);
            return Some(Anomaly::RouteChurn {
                prefix,
                updates_in_window: count,
                threshold,
                severity,
            });
        }
        None
    }

    /// Drop per prefix windows that have gone quiet, bounding memory use. A window
    /// is expired when its most recent timestamp is older than `cutoff`.
    pub fn evict_stale(&mut self, now_secs: f64) {
        let cutoff = now_secs - self.config.window_secs;
        self.windows
            .retain(|_, ring| ring.back().is_some_and(|&t| t >= cutoff));
    }

    fn push_baseline(&mut self, value: f64) {
        if self.baseline.len() >= self.config.baseline_capacity {
            self.baseline.pop_front();
        }
        self.baseline.push_back(value);
    }

    fn threshold(&self) -> f64 {
        if self.baseline.len() < self.config.min_samples {
            // Not enough history yet; require a clearly elevated count.
            return (self.config.min_updates as f64).max(16.0);
        }
        let mut sorted: Vec<f64> = self.baseline.iter().copied().collect();
        let median_count = median(&mut sorted);
        let mut deviations: Vec<f64> = sorted.iter().map(|v| (v - median_count).abs()).collect();
        let mad = median(&mut deviations);
        // 1.4826 scales MAD to be consistent with the standard deviation of a
        // normal distribution.
        median_count + self.config.k * (1.4826 * mad)
    }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn classify(count: f64, threshold: f64) -> Severity {
    if threshold <= 0.0 {
        return Severity::Medium;
    }
    let ratio = count / threshold;
    if ratio >= 2.0 {
        Severity::High
    } else {
        Severity::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn prefix(s: &str) -> IpPrefix {
        IpPrefix::from_str(s).unwrap()
    }

    #[test]
    fn old_timestamps_are_pruned() {
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        let p = prefix("192.0.2.0/24");
        // Two updates far apart: the window never holds more than one.
        assert!(monitor.record(p, 0.0).is_none());
        assert!(monitor.record(p, 100.0).is_none());
        assert_eq!(monitor.windows.get(&p).unwrap().len(), 1);
    }

    #[test]
    fn steady_low_volume_does_not_flag() {
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        let p = prefix("198.51.100.0/24");
        let mut flagged = false;
        for i in 0..200 {
            // One update every 5 seconds keeps the window count near two.
            if monitor.record(p, i as f64 * 5.0).is_some() {
                flagged = true;
            }
        }
        assert!(!flagged);
    }

    #[test]
    fn burst_after_calm_baseline_is_flagged() {
        let cfg = SurgeConfig {
            min_samples: 16,
            ..SurgeConfig::default()
        };
        let mut monitor = SurgeMonitor::new(cfg);

        // Build a calm baseline across many prefixes (low window counts).
        for i in 0..64 {
            let p = prefix(&format!("10.{}.0.0/24", i % 200));
            monitor.record(p, i as f64 * 30.0);
        }

        // Now hammer a single prefix within a tight window.
        let target = prefix("203.0.113.0/24");
        let mut flagged = false;
        for i in 0..60 {
            let now = 10_000.0 + i as f64 * 0.1;
            if let Some(Anomaly::RouteChurn { prefix, .. }) = monitor.record(target, now) {
                assert_eq!(prefix, target);
                flagged = true;
            }
        }
        assert!(flagged, "a tight burst should cross the MAD threshold");
    }

    #[test]
    fn evict_stale_bounds_memory() {
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        monitor.record(prefix("192.0.2.0/24"), 0.0);
        monitor.record(prefix("198.51.100.0/24"), 0.0);
        monitor.evict_stale(1000.0);
        assert!(monitor.windows.is_empty());
    }
}
