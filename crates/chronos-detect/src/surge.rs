//! Temporal surge monitor (blueprint Task 3.3).
//!
//! Route churn means a single prefix whose path keeps changing rapidly at one
//! vantage point. That distinction matters on the RIS Live feed, which is a
//! per peer multiplexed firehose: one real world announcement fans out into one
//! message per collector peer that observed it. Counting updates per prefix
//! alone would therefore conflate "a stable prefix seen by hundreds of peers"
//! with "a prefix flapping rapidly," flagging routine wide visibility as churn.
//!
//! To measure genuine instability we key the sliding window by (prefix, peer):
//! a ring buffer of update timestamps tracks how many updates a single peer saw
//! for a single prefix within the last `window_secs` seconds. That per vantage
//! velocity is compared against an adaptive high pass threshold derived from the
//! Median Absolute Deviation (MAD) of recent per vantage window counts. When a
//! vantage crosses the threshold the prefix is flagged.
//!
//! Detection is edge triggered: a (prefix, peer) that stays above the threshold
//! reports its churn episode once, on the rising edge, rather than on every
//! subsequent update. This reports one anomaly per episode instead of an alert
//! storm for every message while the prefix remains hot.
//!
//! This monitor is single threaded by design; it runs on the single consumer task
//! that drains the ingestion channel, so no internal locking is required.

use crate::anomaly::{Anomaly, Severity};
use chronos_types::{Asn, IpPrefix};
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
    /// A hard floor with two roles: a (prefix, peer) vantage must have at least
    /// this many updates in the window before it can be flagged, and the adaptive
    /// threshold is never allowed to drop below it. The floor is essential on a
    /// real feed: the overwhelming majority of vantages see one or two updates
    /// per window, so the baseline median and MAD both collapse toward zero.
    /// Without the floor the adaptive threshold would fall below a handful of
    /// updates and flag routine activity as churn. A value of 20 updates in a 10
    /// second window (a sustained ~2 updates/second from one peer for one prefix)
    /// marks genuine flapping rather than normal reconvergence.
    pub min_updates: u32,
}

impl Default for SurgeConfig {
    fn default() -> Self {
        Self {
            window_secs: 10.0,
            k: 6.0,
            min_samples: 32,
            baseline_capacity: 1024,
            min_updates: 20,
        }
    }
}

/// The per vantage sliding window plus its edge trigger latch.
#[derive(Default)]
struct Vantage {
    /// Ring buffer of update timestamps within the trailing window.
    ring: VecDeque<f64>,
    /// True while this vantage is currently above the churn threshold; used to
    /// emit one anomaly per episode (on the rising edge) instead of per update.
    flagged: bool,
}

/// Tracks per (prefix, peer) update velocity and flags route churn.
pub struct SurgeMonitor {
    config: SurgeConfig,
    windows: HashMap<(IpPrefix, Asn), Vantage>,
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

    /// Record an update for `prefix` seen by `peer` at `now_secs` (a Unix
    /// timestamp).
    ///
    /// Returns a `RouteChurn` anomaly on the rising edge of a churn episode: when
    /// this vantage's velocity first crosses the adaptive threshold. While the
    /// vantage stays hot no further anomalies are emitted; a fresh episode can be
    /// reported only after it falls back below the threshold.
    pub fn record(&mut self, prefix: IpPrefix, peer: Asn, now_secs: f64) -> Option<Anomaly> {
        let window = self.config.window_secs;
        let cutoff = now_secs - window;

        let vantage = self.windows.entry((prefix, peer)).or_default();
        vantage.ring.push_back(now_secs);
        // Prune timestamps that fell out of the trailing window.
        while vantage.ring.front().is_some_and(|&t| t < cutoff) {
            vantage.ring.pop_front();
        }
        let count = vantage.ring.len() as u32;
        let was_flagged = vantage.flagged;

        // Feed the baseline population used to derive the adaptive threshold.
        self.push_baseline(count as f64);

        if count < self.config.min_updates {
            // Below the floor: the episode (if any) has ended.
            self.windows.get_mut(&(prefix, peer)).unwrap().flagged = false;
            return None;
        }

        let threshold = self.threshold();
        let above = (count as f64) > threshold;
        // Re-borrow after the immutable `threshold()` call to update the latch.
        self.windows.get_mut(&(prefix, peer)).unwrap().flagged = above;

        if above && !was_flagged {
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

    /// Drop per vantage windows that have gone quiet, bounding memory use. A
    /// window is expired when its most recent timestamp is older than `cutoff`.
    pub fn evict_stale(&mut self, now_secs: f64) {
        let cutoff = now_secs - self.config.window_secs;
        self.windows
            .retain(|_, vantage| vantage.ring.back().is_some_and(|&t| t >= cutoff));
    }

    fn push_baseline(&mut self, value: f64) {
        if self.baseline.len() >= self.config.baseline_capacity {
            self.baseline.pop_front();
        }
        self.baseline.push_back(value);
    }

    fn threshold(&self) -> f64 {
        // The `min_updates` floor guards against a degenerate baseline: on a real
        // feed most per prefix window counts are one or two, so both the median
        // and the MAD collapse toward zero and the adaptive term would otherwise
        // sink below a trivial count. Clamping to the floor keeps the detector
        // fixed on genuinely elevated velocities.
        let floor = self.config.min_updates as f64;
        if self.baseline.len() < self.config.min_samples {
            // Not enough history yet; require a clearly elevated count.
            return floor;
        }
        let mut sorted: Vec<f64> = self.baseline.iter().copied().collect();
        let median_count = median(&mut sorted);
        let mut deviations: Vec<f64> = sorted.iter().map(|v| (v - median_count).abs()).collect();
        let mad = median(&mut deviations);
        // 1.4826 scales MAD to be consistent with the standard deviation of a
        // normal distribution.
        (median_count + self.config.k * (1.4826 * mad)).max(floor)
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

    // A single fixed vantage peer for tests that do not exercise peer fan-out.
    const PEER: Asn = Asn(64500);

    #[test]
    fn old_timestamps_are_pruned() {
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        let p = prefix("192.0.2.0/24");
        // Two updates far apart: the window never holds more than one.
        assert!(monitor.record(p, PEER, 0.0).is_none());
        assert!(monitor.record(p, PEER, 100.0).is_none());
        assert_eq!(monitor.windows.get(&(p, PEER)).unwrap().ring.len(), 1);
    }

    #[test]
    fn steady_low_volume_does_not_flag() {
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        let p = prefix("198.51.100.0/24");
        let mut flagged = false;
        for i in 0..200 {
            // One update every 5 seconds keeps the window count near two.
            if monitor.record(p, PEER, i as f64 * 5.0).is_some() {
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
            monitor.record(p, PEER, i as f64 * 30.0);
        }

        // Now hammer a single prefix within a tight window.
        let target = prefix("203.0.113.0/24");
        let mut flagged = false;
        for i in 0..60 {
            let now = 10_000.0 + i as f64 * 0.1;
            if let Some(Anomaly::RouteChurn { prefix, .. }) = monitor.record(target, PEER, now) {
                assert_eq!(prefix, target);
                flagged = true;
            }
        }
        assert!(flagged, "a tight burst should cross the MAD threshold");
    }

    #[test]
    fn peer_fanout_does_not_flag_wide_visibility() {
        // A stable prefix announced once but observed by many distinct peers must
        // not be flagged: each vantage sees only a single update, so no per
        // vantage velocity crosses the floor even though the prefix appears in
        // hundreds of messages.
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        let p = prefix("192.0.2.0/24");
        let mut flagged = false;
        for peer in 0..500u32 {
            if monitor.record(p, Asn(peer + 1), 1_000.0).is_some() {
                flagged = true;
            }
        }
        assert!(!flagged, "wide peer visibility is not churn");
    }

    #[test]
    fn churn_episode_is_reported_once_on_the_rising_edge() {
        // While a vantage stays above the threshold it must report the episode
        // exactly once, not on every subsequent update.
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        for i in 0..4096 {
            let p = prefix(&format!("100.{}.{}.0/24", (i / 256) % 256, i % 256));
            monitor.record(p, Asn((i % 250) + 1), i as f64 * 100.0);
        }

        let target = prefix("198.51.100.0/24");
        let base = 1_000_000.0;
        let mut emissions = 0;
        for i in 0..80 {
            if monitor
                .record(target, PEER, base + i as f64 * 0.1)
                .is_some()
            {
                emissions += 1;
            }
        }
        assert_eq!(emissions, 1, "a sustained episode should emit exactly once");
    }

    #[test]
    fn firehose_singleton_baseline_does_not_flag_moderate_activity() {
        // Reproduce the real feed pathology: a large mass of vantages that each
        // see a single update, driving the baseline median and MAD to zero. A
        // vantage that then reaches the old gate (8 updates) but stays under the
        // floor must not be flagged as churn.
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        for i in 0..4096 {
            let p = prefix(&format!("100.{}.{}.0/24", (i / 256) % 256, i % 256));
            // Each vantage is seen once, far apart, so every window count is one.
            assert!(
                monitor
                    .record(p, Asn((i % 250) + 1), i as f64 * 100.0)
                    .is_none()
            );
        }

        // A vantage reaching 15 updates within the window: elevated versus the
        // singleton mass, but below the sustained flapping floor.
        let target = prefix("203.0.113.0/24");
        let base = 1_000_000.0;
        let mut flagged = false;
        for i in 0..15 {
            if monitor
                .record(target, PEER, base + i as f64 * 0.5)
                .is_some()
            {
                flagged = true;
            }
        }
        assert!(
            !flagged,
            "moderate activity below the floor must not flag despite a collapsed baseline"
        );
    }

    #[test]
    fn sustained_flapping_above_floor_is_flagged() {
        // A single vantage flapping well above the floor (roughly four updates
        // per second) is genuine churn and must be flagged even when the baseline
        // is dominated by singletons.
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        for i in 0..4096 {
            let p = prefix(&format!("100.{}.{}.0/24", (i / 256) % 256, i % 256));
            monitor.record(p, Asn((i % 250) + 1), i as f64 * 100.0);
        }

        let target = prefix("198.51.100.0/24");
        let base = 1_000_000.0;
        let mut flagged = false;
        for i in 0..40 {
            if let Some(Anomaly::RouteChurn { prefix, .. }) =
                monitor.record(target, PEER, base + i as f64 * 0.25)
            {
                assert_eq!(prefix, target);
                flagged = true;
            }
        }
        assert!(flagged, "sustained flapping above the floor should flag");
    }

    #[test]
    fn evict_stale_bounds_memory() {
        let mut monitor = SurgeMonitor::new(SurgeConfig::default());
        monitor.record(prefix("192.0.2.0/24"), PEER, 0.0);
        monitor.record(prefix("198.51.100.0/24"), PEER, 0.0);
        monitor.evict_stale(1000.0);
        assert!(monitor.windows.is_empty());
    }
}
