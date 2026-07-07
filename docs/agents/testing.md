# Testing & CI

The quality gate every change must clear. This mirrors
[.github/workflows/ci.yml](../../.github/workflows/ci.yml).

## Backend

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

- `cargo test --workspace --all-targets` runs unit tests across all seven crates
  plus two integration suites:
  - the **mock RIS Live integration test**
    ([crates/chronos-ingest/tests/mock_stream.rs](../../crates/chronos-ingest/tests/mock_stream.rs)),
    which stands up a mock RIS WebSocket server and exercises subscribe →
    parse → `RisData`;
  - the **end-to-end acceptance suite**
    ([crates/chronos-server/tests/acceptance.rs](../../crates/chronos-server/tests/acceptance.rs)),
    which drives the real Axum router: `/healthz`, `/readyz` (503 until ready,
    then 200), the Prometheus `/metrics` endpoint, and a live WebSocket that
    must emit the seeded snapshot then a broadcast `Delta` over the wire.
- The acceptance suite is why `chronos-server` is a library plus a thin binary
  ([crates/chronos-server/src/lib.rs](../../crates/chronos-server/src/lib.rs)):
  tests import the router and `AppState` directly, and use
  `metrics::standalone_handle()` to avoid the process-global recorder that
  `metrics::install()` registers.
- Scope a run with `cargo test -p <crate>` (e.g. `chronos-detect`),
  `cargo test -p chronos-server --test acceptance`, or `cargo clippy -p <crate>`
  while iterating.

## Live external-data checks (non-gating)

The offline gate above never touches the network. A separate, opt-in suite
([crates/chronos-server/tests/live_data.rs](../../crates/chronos-server/tests/live_data.rs))
exercises the real production data sources so a silent upstream break (an
endpoint outage or a dataset format change) is caught before it reaches users:

- `caida_real_endpoint_downloads_and_parses`: resolves and parses the live CAIDA
  AS-relationship dataset end to end.
- `maxmind_real_db_resolves_prefix`: downloads the real GeoLite2 City and ASN
  databases and resolves a known prefix.

Both are `#[ignore]`d, so `cargo test` skips them by default and they never gate
a PR. Run them explicitly:

```bash
cargo test -p chronos-server --test live_data -- --ignored --nocapture --test-threads=1
```

In CI the `live-data` job in [.github/workflows/ci.yml](../../.github/workflows/ci.yml)
runs them with `continue-on-error`: a failure raises a `::warning::` annotation
worth investigating but does **not** block the build (these endpoints are
external and can be transiently unavailable). Never make a publish/gate job
`needs` this one.

## Production soak (non-gating)

The `chronos-soak` harness ([crates/chronos-soak](../../crates/chronos-soak))
runs the release server against the real RIS Live feed for an extended window
and emits a self-contained Markdown assurance report: the server's own
INFO/WARN/ERROR log summary, resource usage sampled by a separate monitor (RSS,
CPU, RIS socket ingress), and performance (throughput, ingest drop ratio,
anomalies by kind, topology growth, and the WebSocket egress path exercised end
to end). A short window doubles as the resource baseline
([resource-baseline.md](resource-baseline.md)); a long window is the soak.

```bash
cargo build --release --bin chronos-server
# args: duration_secs warmup_secs interval_secs
cargo run --release -p chronos-soak -- 1200 60 10
```

The harness is strongly typed Rust (parsing `/proc`, `ss`, and the Prometheus
exposition with unit-tested helpers) rather than shell, so its logic is covered
by `cargo test -p chronos-soak`. The verdict is PASS, WARN, or FAIL; it exits
non-zero (code 2) only on a genuine crash or panic, treating a dead upstream
feed as a WARN (external and transient), matching the live-data philosophy.

In CI the [.github/workflows/soak.yml](../../.github/workflows/soak.yml) workflow
runs on manual dispatch (with configurable duration/warmup/RIS filter) and on a
weekly schedule. It attaches the report to the job summary and uploads the
report, the CSV time series, and the server log as artifacts. Like `live-data`
it is inherently non-deterministic and MUST NOT gate merges; a FAIL verdict
(crash) surfaces as a failed check, but a WARN does not.

## Frontend

```bash
cd frontend && npm ci && npm run typecheck && npm run lint && npm run build
```

Do NOT run Playwright/Chromium-based E2E in this environment (browser runtime
unavailable); rely on CI for browser tests.

## Environment assumptions

- **No mounted data files in CI.** GeoLite2 and CAIDA datasets are absent, so
  tests must exercise the graceful-degradation paths (disabled geo, degree
  heuristic). Never write a test that requires a `.mmdb` or CAIDA file to be
  present. See [data-files.md](data-files.md).
- **No database, no external network in tests.** The RIS feed is mocked. Tests
  are deterministic and offline.
- The `.github/skills` submodule is agent-only tooling; it is not a build input,
  so CI does not need to check it out to compile or test.

## Publishing workflows

Beyond the CI quality gate, two workflows publish artifacts, each hardened with
a deny-all default `permissions: {}` and jobs that opt into the narrowest scope:

- [.github/workflows/docker-publish.yml](../../.github/workflows/docker-publish.yml)
  builds the container **natively per architecture** (linux/amd64 on
  `ubuntu-latest`, linux/arm64 on `ubuntu-24.04-arm`; no QEMU), pushes each by
  digest, then assembles one multi-arch manifest list on GHCR. `:latest` is
  gated on a green `CI` run on `main`; `v*` tags publish `:stable`, semver, and
  `:sha-*`. The image path is lowercased (`ToolchainOSS/Chronos` →
  `toolchainoss/chronos`) because GHCR rejects uppercase.
- [.github/workflows/release.yml](../../.github/workflows/release.yml) builds
  the `chronos-server` binary natively for linux amd64 and arm64 on `v*` tags.
  The build job is `contents: read` only (it runs untrusted dependency build
  code); a separate `release` job with `contents: write` downloads the
  artifacts and attaches them to the GitHub Release, so the write token never
  touches build code.

## Where tests live

- Unit tests: inline `#[cfg(test)]` modules next to the code (e.g. the `Delta`
  round-trip test in
  [crates/chronos-types/src/delta.rs](../../crates/chronos-types/src/delta.rs)).
- Integration tests: crate `tests/` directories.

## Definition of Done

A change is done when all gate commands above pass locally, tests are
added/updated for changed behavior, any `Delta` protocol change is reflected on
both Rust and TypeScript sides with a round-trip test, and the engine still
starts with data files absent. CI must be green before review.
