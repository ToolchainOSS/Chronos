# Project Chronos

Real-time streaming BGP anomaly detection engine with a hybrid Rust plus React
stack. Chronos ingests the live global BGP feed from RIPE RIS Live, maintains an
in-memory view of internet topology, runs rule-based anomaly heuristics, and
streams minimal delta frames to a browser UI that renders both a logical AS
topology and a geographic impact map.

## Architecture

Chronos is a Cargo workspace of decoupled crates plus a Vite frontend:

```
crates/
  chronos-types      Core primitives: Asn (u32 newtype), compact IpPrefix enum,
                     RIS Live message models, egress Delta frames.
  chronos-ingest     RIPE RIS Live WebSocket client: subscribe, parse, reconnect
                     with backoff, push into a bounded channel (backpressure).
  chronos-topology   Radix (Patricia) trie mapping prefixes to origin ASNs, plus
                     an AS adjacency graph with edge aging.
  chronos-detect     Heuristics: origin hijack, valley-free path leak, and MAD
                     based route churn; pluggable AS relationship providers.
  chronos-geo        Prefix to ISO region resolution via mounted GeoLite2 files.
  chronos-server     Axum WebSocket egress hub, config, metrics, graceful
                     shutdown; wires ingestion, topology, and detection together.
frontend/            React plus TypeScript plus Zustand; react-force-graph-2d for
                     the logical panel and react-map-gl/Maplibre for the map.
```

Data flow:

```
RIS Live  --ws-->  chronos-ingest  --mpsc-->  pipeline (topology + detect)
                                                   |
                                                   v
                                     broadcast Delta frames
                                                   |
                          Axum /ws  <-- fan out -->  browser (Zustand store)
```

The server never ships the full graph over the wire; it emits minimal deltas:
`LinkUp(a, b)`, `LinkDown(a, b)`, and `AreaDegraded(region, severity)`.

## Prerequisites

- Rust (stable, 1.85 or newer; the workspace uses the 2024 edition).
- Node.js 20 or newer (22 recommended) for the frontend.
- Optional: Docker with buildx for container builds.

## Cloning

Agent skills are vendored as a git submodule at `.github/skills`, so clone
recursively:

```bash
git clone --recurse-submodules <repo-url>
# or, on an existing checkout:
git submodule update --init --recursive
```

## Runtime data directory and external datasets

Chronos keeps all volatile runtime state under a single writable data directory
(`CHRONOS_DATA_DIR`, default `/data`). In Docker this is a mounted volume, so a
deployment persists one host path for everything derived at runtime (currently
the cached CAIDA dataset).

External datasets are never committed to source control (they are copyrighted
and/or large binaries). When one is unavailable the corresponding feature
degrades gracefully and the service keeps running.

| Purpose                      | Source                                             | When absent                                            |
| ---------------------------- | -------------------------------------------------- | ------------------------------------------------------ |
| CAIDA AS relationships       | Auto-downloaded and cached (or mounted / pinned)   | Path leak detection uses a degree-based heuristic.     |
| GeoLite2 City (region codes) | Mounted file (`CHRONOS_GEOLITE2_CITY_DB`)          | Region resolution disabled; no `AreaDegraded` deltas.  |
| GeoLite2 ASN (ASN lookup)    | Mounted file (`CHRONOS_GEOLITE2_ASN_DB`)           | ASN geo lookup disabled.                               |

### CAIDA AS relationships (zero configuration)

Path leak detection is most accurate with CAIDA AS relationship data. In the
happy path you configure nothing: on startup Chronos discovers the latest
`as-rel` dataset under
[publicdata.caida.org](https://publicdata.caida.org/datasets/as-relationships/serial-1/),
downloads it once, decompresses it, and caches the plain-text copy under
`<data_dir>/cache/caida/`. The date-stamped filename means a new month is picked
up automatically, and the cached copy is reused on later runs so CAIDA's server
is not hit repeatedly. Acquisition is best-effort and bounded by a timeout; on
any failure (no network, unreachable mirror) Chronos logs a warning and falls
back to the degree heuristic.

Overrides, in order of precedence:

1. `CHRONOS_CAIDA_ASREL`: point at a mounted file (plain `.txt` or `.bz2`).
2. `CHRONOS_CAIDA_URL`: pin an exact dataset URL to fetch and cache (useful for
   a reproducible point-in-time dataset).
3. `CHRONOS_CAIDA_AUTODOWNLOAD=false`: disable auto-download entirely (forces
   the degree heuristic when no file or URL is configured).

### GeoLite2 (MaxMind)

The GeoLite2 City and ASN databases are copyrighted by MaxMind and are large
binary files; they must never be committed. For local development (this project
is already licensed), download them into the data directory and point the
matching variables at them:

```bash
mkdir -p data
curl -L -o data/GeoLite2-City.mmdb https://s.joefang.org/GeoLite2-City
curl -L -o data/GeoLite2-ASN.mmdb  https://s.joefang.org/GeoLite2-ASN
```

The `.gitignore` already excludes `*.mmdb` and the `data/` directory.

## Configuration

All configuration is read from environment variables (see `.env.example`):

| Variable                        | Default                                             | Description                                  |
| ------------------------------- | --------------------------------------------------- | -------------------------------------------- |
| `CHRONOS_BIND_ADDR`             | `0.0.0.0:8080`                                      | HTTP and WebSocket bind address.             |
| `CHRONOS_RIS_URL`               | `ws://ris-live.ripe.net/v1/ws/?client=chronos`     | RIS Live endpoint.                           |
| `CHRONOS_RIS_HOST`              | (unset)                                             | Only this collector (for example `rrc00`).   |
| `CHRONOS_RIS_PATH`              | (unset)                                             | Only updates whose AS path matches (bare ASN = any path via that AS). |
| `CHRONOS_RIS_PREFIX`            | (unset)                                             | Only updates covering this prefix (CIDR).    |
| `CHRONOS_RIS_MORE_SPECIFIC`     | `false`                                             | With a prefix, also match more-specifics (sub-prefix hijacks). |
| `CHRONOS_RIS_LESS_SPECIFIC`     | `false`                                             | With a prefix, also match less-specifics.    |
| `CHRONOS_INGEST_CHANNEL_BOUND`  | `16384`                                             | Bounded ingest channel size (backpressure).  |
| `CHRONOS_BROADCAST_CAPACITY`    | `8192`                                              | Delta broadcast ring buffer capacity.        |
| `CHRONOS_SNAPSHOT_MAX`          | `2000`                                              | Max edges in a new client's initial snapshot.|
| `CHRONOS_EDGE_TTL_SECS`         | `900`                                               | Edge age-out TTL (drives `LinkDown`).        |
| `CHRONOS_SWEEP_INTERVAL_SECS`   | `60`                                                | Interval between edge aging sweeps.          |
| `CHRONOS_DEGREE_RATIO`          | `4.0`                                               | Degree ratio for the fallback heuristic.     |
| `CHRONOS_DATA_DIR`              | `/data`                                             | Writable dir for cached datasets and state.  |
| `CHRONOS_CAIDA_ASREL`           | (unset)                                             | Mounted CAIDA dataset path (`.txt`/`.bz2`).  |
| `CHRONOS_CAIDA_URL`             | (unset)                                             | Exact CAIDA dataset URL to fetch and cache.  |
| `CHRONOS_CAIDA_AUTODOWNLOAD`    | `true`                                              | Auto-discover and cache the latest dataset.  |
| `CHRONOS_CAIDA_BASE_URL`        | CAIDA serial-1 index                                | Directory index for auto-discovery.          |
| `CHRONOS_GEOLITE2_CITY_DB`      | (unset)                                             | Mounted GeoLite2 City path.                  |
| `CHRONOS_GEOLITE2_ASN_DB`       | (unset)                                             | Mounted GeoLite2 ASN path.                   |
| `CHRONOS_HISTORY_ENABLED`       | `false`                                             | Persist anomalies to PostgreSQL (opt-in).    |
| `CHRONOS_HISTORY_URL`           | (unset)                                             | Postgres URL; or set `DATABASE_URL`.         |
| `CHRONOS_HISTORY_RETENTION_DAYS`| `30`                                                | Days of history retained before pruning.     |
| `CHRONOS_HISTORY_MAX_BYTES`     | `2147483648`                                        | Hard cap on Chronos-owned history (2 GiB).   |
| `RUST_LOG`                      | `info`                                              | Tracing filter (via `tracing-subscriber`).   |

### Recommended filter configurations

Chronos subscribes to the RIS Live firehose and, by default, ingests every BGP
update globally (~160 GiB/day; see
[docs/agents/resource-baseline.md](docs/agents/resource-baseline.md)). The RIS
filters below are applied **server-side**, so RIPE never sends what you do not
want: they cut bandwidth and CPU at the source and sharpen signal. Pick the
profile that matches your goal.

**1. Global observatory (default) — full situational awareness.**
Leave all `CHRONOS_RIS_*` filters unset. Highest fidelity: every collector,
every prefix, so the topology graph and the global heuristics (route-leak
detection, degree surges) see the whole internet. Highest cost (~160 GiB/day,
~20% of one core). Use it for research or a well-provisioned backend.

**2. Single vantage point — broad but cheaper.**
```bash
CHRONOS_RIS_HOST=rrc00        # one collector (rrc00 is a large, global peer)
```
Retains broad prefix coverage from one collector's viewpoint at a fraction of
the bandwidth. Trade-off: anomalies only visible from other collectors' peers
may be missed. A good default for resource-constrained deployments.

**3. Watch your own network — everything involving your AS.**
```bash
CHRONOS_RIS_PATH=64500        # your ASN; matches any update whose path uses it
```
Only updates whose AS path traverses your AS. Dramatically lower volume; ideal
for an operator monitoring their own footprint, upstreams, and route leaks that
touch them.

**4. Defend your prefixes — sub-prefix hijack watch (highest signal).**
```bash
CHRONOS_RIS_PREFIX=192.0.2.0/24
CHRONOS_RIS_MORE_SPECIFIC=true   # also catch longer, more-specific announcements
```
Only updates for your prefix and any more-specific announcement inside it. A
classic hijack announces a more-specific of your space to steal traffic, so
this is the canonical low-noise setup to detect an attack on your own address
blocks. Lowest bandwidth of all profiles.

Filters combine (for example a `CHRONOS_RIS_PATH` plus a `CHRONOS_RIS_HOST`
narrows to one collector's view of your AS). Note the trade-off: the tighter
you filter, the narrower the in-memory topology graph, so the global-view
heuristics have less to work with. Filter for targeted monitoring; run
unfiltered (or single-collector) for internet-wide detection. A malformed
`CHRONOS_RIS_PREFIX` fails startup fast rather than silently yielding an empty
feed.

### Persistent anomaly history

By default Chronos is **stateless and in-memory**: it holds live topology and
streams deltas, but keeps no history. Enabling history (`CHRONOS_HISTORY_ENABLED=true`)
durably records every detected anomaly to PostgreSQL so a deployment can offer a
time-series / history view ("show me every hijack that touched my prefix last
month").

What is and is not recorded, by design:

- **Recorded:** anomaly events (hijack, path-leak, route-churn) with their
  timestamp, severity, prefix, origin ASNs, offending AS path, and impacted
  region. This derived signal is tiny compared to the input firehose, so history
  is cheap.
- **Not recorded:** the raw RIS firehose and every `LinkUp`/`LinkDown` flap.
  Re-warehousing raw BGP is what public archives (RIPE RIS, RouteViews) are for;
  Chronos stores the signal it derives, not its input.

**Storage is bounded perpetually.** Events land in one PostgreSQL partition per
UTC day, so retention is enforced by dropping whole day-partitions (an `O(1)`
`DROP TABLE`, never a bloating `DELETE`). Two limits apply together: an age
window (`CHRONOS_HISTORY_RETENTION_DAYS`, default 30) and a hard byte cap
(`CHRONOS_HISTORY_MAX_BYTES`, default 2 GiB). Whichever binds first, the oldest
data is pruned so on-disk size never grows without limit.

**Home lab vs enterprise.** The bundled Postgres (started with
`docker compose --profile history up`) suits a single-user home lab: keep the
2 GiB cap, or lower it. For an enterprise that wants unbounded, high-granularity
history, do not raise the cap; instead scrape `/metrics` into your own Prometheus
(or remote-write to a long-term TSDB). That data lives in your platform under
your retention, entirely outside the Chronos-owned 2 GiB budget: Chronos
produces, your observability stack stores.

**Graceful and off the hot path.** History is opt-in, connects lazily (startup
never blocks on the database), and writes on a dedicated background task fed by a
bounded, drop-on-full channel. If the database is slow or unreachable, anomaly
records are dropped and counted (`chronos_history_events_dropped_total`); the
detection engine never stalls.

## Running locally

Backend:

```bash
# Zero config: Chronos auto-downloads the CAIDA dataset into ./data on startup.
export CHRONOS_DATA_DIR=$PWD/data
# Optional geo enrichment (licensed GeoLite2 files):
# export CHRONOS_GEOLITE2_CITY_DB=$PWD/data/GeoLite2-City.mmdb
# export CHRONOS_GEOLITE2_ASN_DB=$PWD/data/GeoLite2-ASN.mmdb
cargo run -p chronos-server --release
```

Frontend (dev server proxies `/ws` to the backend on port 8080):

```bash
cd frontend
npm install
npm run dev
```

Then open the printed Vite URL (default `http://localhost:5173`).

## Endpoints

- `GET /ws`: WebSocket delta stream (JSON `Delta` frames).
- `GET /healthz`: liveness.
- `GET /readyz`: readiness.
- `GET /metrics`: Prometheus metrics.

## Docker

```bash
# Build for OCI Ampere A1 (ARM64):
docker buildx build --platform linux/arm64 -t chronos:arm64 -f deploy/Dockerfile .

# Or run with compose (mounts ./data read-write into /data for the cache):
cp .env.example .env
docker compose up --build
```

## Testing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # includes a mock RIS Live integration test

cd frontend && npm run typecheck && npm run lint && npm run build
```

Note: GeoLite2 and CAIDA files are absent in CI (never committed), so tests
exercise the disabled-geo and heuristic-relationship fallback paths.

## For AI agents

This repository is configured for a vendor-neutral, agentic development
workflow. Start at [AGENTS.md](AGENTS.md): it is the always-on operating manual
(tooling, boundaries, definition of done) and links out to progressive-
disclosure domain docs under [docs/agents/](docs/agents/) that you load
on demand:

- [engineering-standards.md](docs/agents/engineering-standards.md) - full standards & output contract.
- [pipeline.md](docs/agents/pipeline.md) - RIS ingestion, bounded channel, consumer.
- [detection.md](docs/agents/detection.md) - topology structures & anomaly heuristics.
- [data-files.md](docs/agents/data-files.md) - mounted GeoLite2/CAIDA files & config.
- [egress-frontend.md](docs/agents/egress-frontend.md) - Delta protocol, Axum hub, React/Zustand.
- [testing.md](docs/agents/testing.md) - quality gate & CI.

Reusable agent skills live in the [`.github/skills`](.github/skills) submodule.

## Style

Source comments and docs avoid em dashes; they use colons, semicolons, and
parentheses instead.

## License

MIT. See [LICENSE](LICENSE).
