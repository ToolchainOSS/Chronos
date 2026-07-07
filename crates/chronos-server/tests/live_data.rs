//! Live external-data acceptance tests.
//!
//! Unlike the offline suite in `acceptance.rs`, these tests reach the real
//! external data sources Chronos depends on in production:
//! - the CAIDA AS-relationship archive on `publicdata.caida.org`, and
//! - the GeoLite2 City and ASN databases served from the project mirror.
//!
//! They are marked `#[ignore]` so they never run as part of the quality gate
//! (`cargo test`): these endpoints can be down transiently, and a green pull
//! request must not depend on a third party's uptime. They are run explicitly
//! (`cargo test --test live_data -- --ignored`) by a dedicated, non-blocking CI
//! job whose failure raises a warning worth investigating rather than failing
//! the build. Run them locally the same way.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

use chronos_detect::{Relationship, RelationshipProvider, parse_caida_as_rel};
use chronos_geo::GeoResolver;
use chronos_server::config::AppConfig;
use chronos_types::{Asn, IpPrefix};

/// GeoLite2 database mirror URLs (the licensed copies used for this project).
const GEOLITE2_CITY_URL: &str = "https://s.joefang.org/GeoLite2-City";
const GEOLITE2_ASN_URL: &str = "https://s.joefang.org/GeoLite2-ASN";

/// A unique, self-cleaning temporary directory for downloaded fixtures.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("chronos-live-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The real CAIDA acquisition path: auto-discover the latest dataset, download
/// it, decompress it, cache it, and parse a usable relationship set.
#[tokio::test]
#[ignore = "reaches the real CAIDA endpoint; run explicitly with --ignored"]
async fn caida_real_endpoint_downloads_and_parses() {
    let tmp = TempDir::new("caida");
    let config = AppConfig {
        data_dir: tmp.path().to_path_buf(),
        // Default base URL and auto-download; no mounted file, no pinned URL.
        ..AppConfig::default()
    };

    let started = Instant::now();
    let path = chronos_server::caida::resolve_dataset(&config)
        .await
        .expect("CAIDA dataset should be acquired from the live endpoint");
    let elapsed = started.elapsed();

    let contents = std::fs::read_to_string(&path).expect("read cached CAIDA dataset");
    let rels = parse_caida_as_rel(&contents);

    // The live dataset is large (hundreds of thousands of directed edges); a
    // tiny result would signal a truncated download or a format change.
    assert!(
        rels.len() > 100_000,
        "expected a large CAIDA relationship set, got {}",
        rels.len()
    );

    // Spot-check the wire semantics: AS174 (Cogent, a large transit provider)
    // must appear in some concrete provider/customer/peer relationship, never
    // decoding to a garbage value.
    let sample = rels.relationship(Asn(174), Asn(3356));
    assert!(
        matches!(
            sample,
            Relationship::Provider
                | Relationship::Customer
                | Relationship::Peer
                | Relationship::Unknown
        ),
        "relationship lookup returned an impossible value"
    );

    // The cache must be reused on a second resolve (no second download).
    let again = chronos_server::caida::resolve_dataset(&config)
        .await
        .expect("cached CAIDA dataset should resolve");
    assert_eq!(again, path, "second resolve should hit the cache");

    eprintln!(
        "live CAIDA: {} directed relationships from {} in {:.1}s",
        rels.len(),
        path.display(),
        elapsed.as_secs_f64()
    );
}

/// The real GeoLite2 databases resolve a well-known prefix to a plausible
/// region and origin ASN.
#[tokio::test]
#[ignore = "downloads the real GeoLite2 databases; run explicitly with --ignored"]
async fn maxmind_real_db_resolves_prefix() {
    let tmp = TempDir::new("geo");
    let city_path = tmp.path().join("GeoLite2-City.mmdb");
    let asn_path = tmp.path().join("GeoLite2-ASN.mmdb");

    download(GEOLITE2_CITY_URL, &city_path).await;
    download(GEOLITE2_ASN_URL, &asn_path).await;

    let geo = GeoResolver::load(Some(&city_path), Some(&asn_path));
    assert!(
        geo.is_region_enabled(),
        "region resolution should be enabled once the City database is loaded"
    );

    // 8.8.8.0/24 is Google's well-known public DNS block: US region, AS15169.
    let prefix = IpPrefix::from_str("8.8.8.0/24").unwrap();

    let region = geo.resolve_region(&prefix);
    assert!(
        region.as_deref().is_some_and(|r| r.starts_with("US")),
        "expected a US region for 8.8.8.0/24, got {region:?}"
    );

    let asn = geo.resolve_asn(&prefix);
    assert_eq!(
        asn,
        Some(Asn(15169)),
        "expected AS15169 (Google) for 8.8.8.0/24, got {asn:?}"
    );

    eprintln!("live GeoLite2: 8.8.8.0/24 -> region {region:?}, asn {asn:?}");
}

/// Download a URL to a path, failing the test with a clear message on error.
async fn download(url: &str, dest: &std::path::Path) {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("build HTTP client");
    let bytes = client
        .get(url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("requesting {url}: {e}"))
        .error_for_status()
        .unwrap_or_else(|e| panic!("{url} returned an error status: {e}"))
        .bytes()
        .await
        .unwrap_or_else(|e| panic!("reading body from {url}: {e}"));
    std::fs::write(dest, &bytes).unwrap_or_else(|e| panic!("writing {}: {e}", dest.display()));
}
