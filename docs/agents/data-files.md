# Runtime Data Directory & Configuration

Chronos has **no database**. All volatile runtime state lives under one writable
data directory (`CHRONOS_DATA_DIR`, default `/data`), which a deployment mounts
to a host path. The CAIDA AS-relationship dataset is acquired automatically and
cached there; GeoLite2 databases are optional mounted files. Read this before
touching config, geo resolution, or the relationship provider.

## The golden rule

**Never commit or bundle external datasets.** GeoLite2 (`*.mmdb`) is copyrighted
and large; CAIDA `as-rel` files are large and carry CAIDA's terms. They stay
gitignored (`*.mmdb`, `/data/`, `GeoLite2-*`, `*as-rel*`) and are downloaded or
mounted at runtime. The engine MUST start and run without any of them, degrading
gracefully. The cache under `<data_dir>/cache/` is derived state, not a source
artifact: it is safe to delete and will be repopulated.

## The data directory

`CHRONOS_DATA_DIR` (default `/data`) is the single writable location for cached
datasets and any future volatile state. It must be writable by the non-root
runtime user; the Dockerfile creates and chowns `/data` and marks it a `VOLUME`,
and docker-compose mounts `./data:/data` read-write. Keep new runtime-derived
state under this directory so a single host mount captures everything.

## The datasets

| Dataset | Source | Env vars | When absent |
|---|---|---|---|
| CAIDA AS-relationship | Auto-downloaded + cached, or mounted, or pinned URL | `CHRONOS_CAIDA_ASREL`, `CHRONOS_CAIDA_URL`, `CHRONOS_CAIDA_AUTODOWNLOAD`, `CHRONOS_CAIDA_BASE_URL` | Falls back to `DegreeHeuristic` |
| GeoLite2 City `.mmdb` | Mounted file | `CHRONOS_GEOLITE2_CITY_DB` | Geo resolution skipped; no `AreaDegraded` frames |
| GeoLite2 ASN `.mmdb` | Mounted file | `CHRONOS_GEOLITE2_ASN_DB` | Enrichment skipped |

### CAIDA acquisition

[crates/chronos-server/src/caida.rs](../../crates/chronos-server/src/caida.rs)
resolves a plain-text `as-rel` file, in precedence order: a mounted
`CHRONOS_CAIDA_ASREL` file (decompressed if `.bz2`), then a pinned
`CHRONOS_CAIDA_URL`, then (when `CHRONOS_CAIDA_AUTODOWNLOAD` is on, the default)
auto-discovery of the newest `YYYYMMDD.as-rel[2].txt.bz2` under
`CHRONOS_CAIDA_BASE_URL`. Downloads are decompressed and cached atomically under
`<data_dir>/cache/caida/`; an existing cache entry is reused. `resolve_dataset`
never returns an error and is bounded by a timeout: every failure degrades to
`None` and the degree heuristic. The parser
([crates/chronos-detect/src/relationships.rs](../../crates/chronos-detect/src/relationships.rs))
splits on `|` and ignores trailing columns, so both serial-1 (`as1|as2|rel`) and
serial-2 (`as1|as2|rel|source`) formats parse.

## Graceful degradation (mandatory behavior)

- `GeoResolver::load(city, asn)` opens whatever is present;
  `GeoResolver::disabled()` is a valid no-op resolver. `resolve_region` returns
  `None` when geo is unavailable: callers must handle `None`, never `unwrap`.
  A missing-address lookup is `AddressNotFoundError`, not a fatal error. See
  [crates/chronos-geo/src/lib.rs](../../crates/chronos-geo/src/lib.rs).
- `build_relationship_provider` uses the CAIDA dataset resolved by
  `caida::resolve_dataset` (auto-download, mount, or pinned URL) when it parses
  to a non-empty set, else the degree heuristic. See
  [crates/chronos-server/src/main.rs](../../crates/chronos-server/src/main.rs)
  and [crates/chronos-server/src/caida.rs](../../crates/chronos-server/src/caida.rs).

Do not add a code path that panics or fails startup because an optional dataset
is unavailable. CI runs without mounted files and without asserting network
access, so tests must rely on the fallbacks.

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
| `CHRONOS_DATA_DIR` | `/data` | Writable dir for cached datasets and volatile state |
| `CHRONOS_CAIDA_ASREL` | (unset) | Path to a mounted CAIDA dataset (`.txt`/`.bz2`) |
| `CHRONOS_CAIDA_URL` | (unset) | Exact CAIDA dataset URL to fetch and cache |
| `CHRONOS_CAIDA_AUTODOWNLOAD` | `true` | Auto-discover and cache the latest dataset |
| `CHRONOS_CAIDA_BASE_URL` | CAIDA serial-1 index | Directory index for auto-discovery |
| `CHRONOS_GEOLITE2_CITY_DB` | (unset) | Path to mounted GeoLite2 City db |
| `CHRONOS_GEOLITE2_ASN_DB` | (unset) | Path to mounted GeoLite2 ASN db |

When adding a new tunable: add the field + default in `config.rs`, parse it in
`from_env` with a validating error message, and document it in `.env.example`.
Never read `env::var` scattered across the codebase: config lives in one place.
