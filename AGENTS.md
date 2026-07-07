# AGENTS.md

Chronos is a real-time BGP anomaly detection engine: a Rust workspace that
ingests the RIPE RIS Live feed, maintains in-memory internet topology, runs
rule-based anomaly heuristics, and streams minimal delta frames to a React
(Vite + TypeScript + Zustand) frontend over WebSockets. The engine is
**stateless and in-memory by default** (no database on the hot path); its
runtime inputs are read-only data files mounted at runtime. Persisting anomaly
history to PostgreSQL is an **opt-in** feature (`CHRONOS_HISTORY_ENABLED`,
crate `chronos-history`): off by default, lazily connected, written off the hot
path, and bounded by retention plus a hard byte cap.

This file is the always-on operating manual for coding agents. It is
intentionally short; load the linked domain docs just-in-time for deeper work.

## Tooling & Commands

- Backend: `cargo` (stable, with `rustfmt` + `clippy`). Frontend: `npm`
  (Node 20+). Docker (with buildx) is optional, for container builds.
- Agent skills live in the `.github/skills` git submodule. Clone with
  `git clone --recurse-submodules`, or run
  `git submodule update --init --recursive` on an existing checkout.

```bash
# Backend quality gate (matches CI)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets  # includes the mock RIS integration test + e2e acceptance suite

# Frontend quality gate
cd frontend && npm ci && npm run typecheck && npm run lint && npm run build
```

Scoped runs: `cargo test -p <crate>` (e.g. `chronos-detect`);
`cargo clippy -p <crate>`. Full test / CI / Docker details →
[docs/agents/testing.md](docs/agents/testing.md).

## Boundaries & Constraints

- **Never commit data files or secrets.** GeoLite2 `.mmdb` (copyrighted, large)
  and CAIDA AS-relationship datasets are mounted at runtime, never bundled or
  committed. They stay gitignored; configure them by env var. →
  [docs/agents/data-files.md](docs/agents/data-files.md).
- **Preserve the Delta wire contract.** The Rust `Delta` enum and the frontend
  `Delta` type are one shared protocol. If you change one, change the other in
  the same commit and add a round-trip test. This is the one hard exception to
  "ruthless refactoring": treat cross-boundary compatibility like data
  integrity. → [docs/agents/egress-frontend.md](docs/agents/egress-frontend.md).
- **Never block the ingestion socket.** The RIS reader pushes into a *bounded*
  channel and drops on full (backpressure); do not swap in an unbounded channel
  or perform blocking/heavy work in the read loop. Do detection work on the
  consumer side. → [docs/agents/pipeline.md](docs/agents/pipeline.md).
- **Degrade gracefully when data is absent.** Missing GeoLite2 disables
  `AreaDegraded` (do not panic); missing CAIDA falls back to the degree
  heuristic. Never make startup hard-fail on an unmounted optional file. →
  [docs/agents/data-files.md](docs/agents/data-files.md).
- **Don't hand-tune metrics/logging into hot loops.** Use the existing
  `tracing` spans and `metrics` counters; do not add per-message allocations or
  `println!`/`dbg!` to the consumer path.
- **Frontend: don't open raw `WebSocket`s in components.** Use the shared client
  in [frontend/src/ws.ts](frontend/src/ws.ts) and the Zustand store; components
  subscribe to store slices. → [docs/agents/egress-frontend.md](docs/agents/egress-frontend.md).
- **Never edit CI workflows** under `.github/workflows/` unless explicitly
  asked, and never print env-configured secrets or full DB/creds to logs.

## Definition of Done

Work is complete only when the quality-gate commands above pass and tests are
added/updated for changed behavior. Any change to the `Delta` protocol updates
both the Rust and TypeScript sides plus a round-trip test. CI must be green
before review.

## Agent Operating Contract

Every session follows the standards in
[docs/agents/engineering-standards.md](docs/agents/engineering-standards.md).
Key always-on rules:

- **Operate autonomously.** Make the most reasonable assumption on ambiguity,
  document it, and proceed; only pause for destructive/irreversible actions.
  Announce explicit completion: do not stop silently.
- **Make invalid states unrepresentable**; push invariants into the type system
  (e.g. `Asn` newtype, the `IpPrefix` enum, the tagged `Delta` enum).
- **Refactor ruthlessly** (no internal backward-compat duty) and prune dead
  code: except the Delta wire contract and mounted-data behavior, where
  cross-boundary compatibility is non-negotiable.
- **Keep files ≤ ~500 lines**; split into cohesive submodules as they grow.
- **Handle errors idiomatically** (`Result`/`Option`, `?`, `thiserror`,
  `anyhow` at boundaries); never swallow them.
- **Avoid reflexive `.clone()`/`Rc`/`Box<dyn _>`** to appease the borrow
  checker: redesign data flow instead. Justified shared ownership (e.g.
  `Arc<AsGraph>` across tasks, the broadcast sender) remains fine.

## Agent Skills

Reusable, project-agnostic agent skills are vendored as a **git submodule** at
[.github/skills](.github/skills), tracking
[BTreeMap/SKILLs](https://github.com/BTreeMap/SKILLs). Each skill is a
self-contained `SKILL.md`: load it on demand when its description matches the
task. **Hand-authored commit messages MUST follow the
[git-commits](.github/skills/git-commits/SKILL.md) skill.**

- **Don't edit skills in place.** The submodule is read-only here; propose
  changes upstream in `BTreeMap/SKILLs`, then bump the pointer.
- **Sync skills** by advancing the submodule and committing the new pointer:
  ```bash
  git submodule update --remote .github/skills
  git commit -m "chore(skills): Bump skills submodule"
  ```
- **Fresh checkouts** must init submodules (`git clone --recurse-submodules` or
  `git submodule update --init --recursive`). The submodule is agent-only
  tooling and is not required to build or test, so CI does not check it out.

## Domain Documentation (load on demand)

| When working on… | Read |
|---|---|
| RIS ingestion, bounded channel, reconnect/backpressure, consumer pipeline | [docs/agents/pipeline.md](docs/agents/pipeline.md) |
| Topology (radix trie, AS graph) & detection heuristics | [docs/agents/detection.md](docs/agents/detection.md) |
| Mounted GeoLite2 / CAIDA data files, env config, graceful degradation | [docs/agents/data-files.md](docs/agents/data-files.md) |
| Axum WebSocket egress, Delta protocol, React/Zustand frontend | [docs/agents/egress-frontend.md](docs/agents/egress-frontend.md) |
| Tests, CI gate, mock RIS integration, acceptance checks | [docs/agents/testing.md](docs/agents/testing.md) |
| Bandwidth, CPU, and memory baseline for a running instance | [docs/agents/resource-baseline.md](docs/agents/resource-baseline.md) |
| Full engineering standards & output contract | [docs/agents/engineering-standards.md](docs/agents/engineering-standards.md) |
| Reusable agent skills (commit style, authoring) | [.github/skills/README.md](.github/skills/README.md) |
