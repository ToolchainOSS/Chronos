//! Prefix to ISO region resolution using mounted MaxMind GeoLite2 databases.
//!
//! Data handling (important):
//! - The GeoLite2 City and ASN databases (`.mmdb`) are copyrighted by MaxMind and
//!   are large binaries. They are NEVER committed to source control; they are
//!   mounted into the container at runtime.
//! - The paths are supplied through configuration (environment variables
//!   `CHRONOS_GEOLITE2_CITY_DB` and `CHRONOS_GEOLITE2_ASN_DB`; see the server
//!   crate and the README).
//! - When a database path is unset or the file cannot be opened, geo resolution
//!   is disabled gracefully: the resolver returns `None` and the engine keeps
//!   running (it simply does not emit `AreaDegraded` deltas). A warning is logged.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use chronos_types::{Asn, IpPrefix};
use ipnetwork::IpNetwork;
use maxminddb::{geoip2, MaxMindDBError, Reader};
use serde::Deserialize;
use std::net::IpAddr;
use std::path::Path;
use tracing::{info, warn};

/// Resolves prefixes to ISO region codes and (optionally) ASNs using GeoLite2.
#[derive(Default)]
pub struct GeoResolver {
    city: Option<Reader<Vec<u8>>>,
    asn: Option<Reader<Vec<u8>>>,
}

impl GeoResolver {
    /// A disabled resolver (used when no databases are configured).
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Load the resolver from optional mounted database paths.
    ///
    /// This never returns an error: a missing or unreadable database simply
    /// disables that lookup (a warning is logged). This keeps the service running
    /// when the operator has not mounted the databases.
    pub fn load(city_path: Option<&Path>, asn_path: Option<&Path>) -> Self {
        let city = open_optional("GeoLite2 City", city_path);
        let asn = open_optional("GeoLite2 ASN", asn_path);
        Self { city, asn }
    }

    /// True when at least the City database is available (region resolution can
    /// produce results).
    pub fn is_region_enabled(&self) -> bool {
        self.city.is_some()
    }

    /// Resolve a prefix to an ISO region code.
    ///
    /// Returns a subdivision code when available (for example `US-CA`), otherwise
    /// a country code (for example `US`), otherwise `None`.
    pub fn resolve_region(&self, prefix: &IpPrefix) -> Option<String> {
        let reader = self.city.as_ref()?;
        let ip = prefix_ip(prefix);
        let city: geoip2::City = lookup(reader, ip)?;

        let country_code = city
            .country
            .as_ref()
            .and_then(|c| c.iso_code)
            .map(str::to_owned);

        let subdivision_code = city
            .subdivisions
            .as_ref()
            .and_then(|subs| subs.first())
            .and_then(|s| s.iso_code);

        match (country_code, subdivision_code) {
            (Some(country), Some(sub)) => Some(format!("{country}-{sub}")),
            (Some(country), None) => Some(country),
            _ => None,
        }
    }

    /// Resolve the ASN that GeoLite2 associates with a prefix, when the ASN
    /// database is available.
    pub fn resolve_asn(&self, prefix: &IpPrefix) -> Option<Asn> {
        let reader = self.asn.as_ref()?;
        let ip = prefix_ip(prefix);
        let record: geoip2::Asn = lookup(reader, ip)?;
        record.autonomous_system_number.map(Asn)
    }
}

fn open_optional(label: &str, path: Option<&Path>) -> Option<Reader<Vec<u8>>> {
    let path = path?;
    match Reader::open_readfile(path) {
        Ok(reader) => {
            info!(database = label, path = %path.display(), "geo: loaded database");
            Some(reader)
        }
        Err(err) => {
            warn!(
                database = label,
                path = %path.display(),
                %err,
                "geo: failed to open database; disabling this lookup"
            );
            None
        }
    }
}

fn lookup<'de, T: Deserialize<'de>>(reader: &'de Reader<Vec<u8>>, ip: IpAddr) -> Option<T> {
    match reader.lookup::<T>(ip) {
        Ok(record) => Some(record),
        Err(MaxMindDBError::AddressNotFoundError(_)) => None,
        Err(err) => {
            warn!(%err, %ip, "geo: lookup error");
            None
        }
    }
}

fn prefix_ip(prefix: &IpPrefix) -> IpAddr {
    match IpNetwork::from(*prefix) {
        IpNetwork::V4(v4) => IpAddr::V4(v4.network()),
        IpNetwork::V6(v6) => IpAddr::V6(v6.network()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn disabled_resolver_returns_none() {
        let resolver = GeoResolver::disabled();
        assert!(!resolver.is_region_enabled());
        let p = IpPrefix::from_str("192.0.2.0/24").unwrap();
        assert!(resolver.resolve_region(&p).is_none());
        assert!(resolver.resolve_asn(&p).is_none());
    }

    #[test]
    fn missing_paths_disable_gracefully() {
        let resolver = GeoResolver::load(
            Some(Path::new("/nonexistent/City.mmdb")),
            Some(Path::new("/nonexistent/ASN.mmdb")),
        );
        assert!(!resolver.is_region_enabled());
    }

    #[test]
    fn prefix_ip_extracts_network_address() {
        let v4 = IpPrefix::from_str("192.0.2.128/25").unwrap();
        assert_eq!(prefix_ip(&v4).to_string(), "192.0.2.128");
        let v6 = IpPrefix::from_str("2001:db8::/32").unwrap();
        assert_eq!(prefix_ip(&v6).to_string(), "2001:db8::");
    }
}
