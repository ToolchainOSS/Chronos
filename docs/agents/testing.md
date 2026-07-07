# Testing & CI

The quality gate every change must clear. This mirrors
[.github/workflows/ci.yml](../../.github/workflows/ci.yml).

## Backend

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- `cargo test --workspace` runs unit tests across all six crates plus the
  **mock RIS Live integration test**
  ([crates/chronos-ingest/tests/mock_stream.rs](../../crates/chronos-ingest/tests/mock_stream.rs)),
  which stands up a mock RIS WebSocket server and exercises subscribe → parse →
  `RisData`.
- Scope a run with `cargo test -p <crate>` (e.g. `chronos-detect`) or
  `cargo clippy -p <crate>` while iterating.

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
