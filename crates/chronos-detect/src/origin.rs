//! Origin validation heuristic (blueprint Task 3.1).

use crate::anomaly::{Anomaly, Severity};
use chronos_topology::OriginObservation;
use chronos_types::IpPrefix;

/// Evaluate an origin observation for a prefix.
///
/// Returns a `PrefixHijack` anomaly when the prefix was previously announced by a
/// different origin ASN. Severity scales with how many distinct origins have been
/// seen: a first flip is `Medium`; repeated flapping between many origins is
/// `High` (persistent instability or an ongoing dispute).
pub fn check_origin(prefix: &IpPrefix, obs: &OriginObservation) -> Option<Anomaly> {
    if !obs.is_possible_hijack() {
        return None;
    }
    let previous_origin = obs.previous_origin?;
    let severity = if obs.distinct_origins >= 3 {
        Severity::High
    } else {
        Severity::Medium
    };
    Some(Anomaly::PrefixHijack {
        prefix: *prefix,
        previous_origin,
        new_origin: obs.current_origin,
        severity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronos_types::Asn;
    use std::str::FromStr;

    fn prefix() -> IpPrefix {
        IpPrefix::from_str("192.0.2.0/24").unwrap()
    }

    #[test]
    fn no_anomaly_on_first_sighting() {
        let obs = OriginObservation {
            previous_origin: None,
            current_origin: Asn(64500),
            changed: false,
            distinct_origins: 1,
        };
        assert!(check_origin(&prefix(), &obs).is_none());
    }

    #[test]
    fn flags_medium_on_first_flip() {
        let obs = OriginObservation {
            previous_origin: Some(Asn(64500)),
            current_origin: Asn(64510),
            changed: true,
            distinct_origins: 2,
        };
        let anomaly = check_origin(&prefix(), &obs).unwrap();
        assert_eq!(anomaly.severity(), Severity::Medium);
    }

    #[test]
    fn flags_high_on_repeated_flapping() {
        let obs = OriginObservation {
            previous_origin: Some(Asn(64500)),
            current_origin: Asn(64520),
            changed: true,
            distinct_origins: 4,
        };
        let anomaly = check_origin(&prefix(), &obs).unwrap();
        assert_eq!(anomaly.severity(), Severity::High);
    }
}
