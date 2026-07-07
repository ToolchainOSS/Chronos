# Mounted Data Files & Configuration

Chronos has **no database**. Its only persistent inputs are read-only data files
mounted at runtime, and everything is configured by environment variable. Read
this before touching config, geo resolution, or the relationship provider.

## The golden rule

**Never commit, bundle, or hard-require these files.** They are large,
externally licensed, and change independently of the code. They stay gitignored
(`*.mmdb`, `/data/`, `GeoLite2-*`, `*as-rel*`) and are mounted at runtime. The
engine MUST start and run without them, degrading gracefully.

## The files

| File | Env var | Purpose | When absent |
|---|---|---|---|
| GeoLite2 City `.mmdb` | `CHRONOS_GEOLITE2_CITY_DB` | Map prefixes → region for `AreaDegraded` | Geo resolution skipped; no `AreaDegraded` frames |
| GeoLite2 ASN `.mmdb` | `CHRONOS_GEOLITE2_ASN_DB` | ASN → org/geo enrichment | Enrichment skipped |
| CAIDA AS-relationship | `CHRONOS_CAIDA_ASREL` | Feed `CaidaRelationships` for valley-free path-leak detection | Falls back to `DegreeHeuristic` |

GeoLite2 (MaxMind) and CAIDA datasets carry their own licenses. Users mount
their own copies; Chronos ships neither.

## Graceful degradation (mandatory behavior)

- `GeoResolver::load(city, asn)` opens whatever is present;
  `GeoResolver::disabled()` is a valid no-op resolver. `resolve_region` returns
  `None` when geo is unavailable: callers must handle `None`, never `unwrap`.
  A missing-address lookup is `AddressNotFoundError`, not a fatal error. See
  [crates/chronos-geo/src/lib.rs](../../crates/chronos-geo/src/lib.rs).
- `build_relationship_provider` picks CAIDA if `CHRONOS_CAIDA_ASREL` is set and
  parseable, else the degree heuristic. See
  [crates/chronos-server/src/main.rs](../../crates/chronos-server/src/main.rs).

Do not add a code path that panics or fails startup because an optional file is
unmounted. CI runs without these files, so tests must rely on the fallbacks.

## Full configuration surface

All knobs are `CHRONOS_*` env vars with defaults in
[crates/chronos-server/src/config.rs](../../crates/chronos-server/src/config.rs)
and documented in [.env.example](../../.env.example):

| Var | Default | Meaning |
|---|---|---|
| `CHRONOS_BIND_ADDR` | `0.0.0.0:8080` | HTTP + WS bind address |
| `CHRONOS_RIS_URL` | RIS Live stream URL | Upstream feed |
| `CHRONOS_RIS_HOST` | (unset) | Optional collector host filter |
| `CHRONOS_INGEST_CHANNEL_BOUND` | `16384` | Bounded ingest channel size |
| `CHRONOS_BROADCAST_CAPACITY` | `8192` | Broadcast ring-buffer capacity |
| `CHRONOS_SNAPSHOT_MAX` | `2000` | Max edges in initial client snapshot |
| `CHRONOS_EDGE_TTL_SECS` | `900` | Edge age-out TTL (drives `LinkDown`) |
| `CHRONOS_SWEEP_INTERVAL_SECS` | `60` | Edge-aging sweep interval |
| `CHRONOS_DEGREE_RATIO` | `4.0` | Degree-heuristic ratio (CAIDA fallback) |
| `CHRONOS_GEOLITE2_CITY_DB` | (unset) | Path to mounted GeoLite2 City db |
| `CHRONOS_GEOLITE2_ASN_DB` | (unset) | Path to mounted GeoLite2 ASN db |
| `CHRONOS_CAIDA_ASREL` | (unset) | Path to mounted CAIDA AS-rel dataset |

When adding a new tunable: add the field + default in `config.rs`, parse it in
`from_env` with a validating error message, and document it in `.env.example`.
Never read `env::var` scattered across the codebase: config lives in one place.
